//! Tests for registry composition features (merge, builder_from, register_all)

use duroxide::providers::sqlite::SqliteProvider;
use duroxide::runtime;
use duroxide::runtime::registry::{ActivityRegistry, ActivityRegistryBuilder};
use duroxide::{ActivityContext, Client, OrchestrationContext, OrchestrationRegistry, OrchestrationStatus};
use std::sync::{Arc, Mutex};

// Helper orchestrations
async fn orch1(_ctx: OrchestrationContext, input: String) -> Result<String, String> {
    Ok(format!("orch1: {}", input))
}

async fn orch2(_ctx: OrchestrationContext, input: String) -> Result<String, String> {
    Ok(format!("orch2: {}", input))
}

async fn orch3(_ctx: OrchestrationContext, input: String) -> Result<String, String> {
    Ok(format!("orch3: {}", input))
}

// Helper activities
async fn activity1(_ctx: ActivityContext, input: String) -> Result<String, String> {
    Ok(format!("activity1: {}", input))
}

async fn activity2(_ctx: ActivityContext, input: String) -> Result<String, String> {
    Ok(format!("activity2: {}", input))
}

async fn activity3(_ctx: ActivityContext, input: String) -> Result<String, String> {
    Ok(format!("activity3: {}", input))
}

#[test]
fn test_orchestration_registry_merge() {
    // Create first registry
    let registry1 = OrchestrationRegistry::builder()
        .register("orch1", orch1)
        .register("orch2", orch2)
        .build();

    // Create second registry
    let registry2 = OrchestrationRegistry::builder().register("orch3", orch3).build();

    // Merge both into a new registry
    let combined = OrchestrationRegistry::builder()
        .merge(registry1)
        .merge(registry2)
        .build();

    // Verify all three orchestrations are present
    let names = combined.list_orchestration_names();
    assert_eq!(names.len(), 3);
    assert!(names.contains(&"orch1".to_string()));
    assert!(names.contains(&"orch2".to_string()));
    assert!(names.contains(&"orch3".to_string()));
}

#[test]
fn test_orchestration_registry_builder_from() {
    // Create base registry
    let base = OrchestrationRegistry::builder()
        .register("orch1", orch1)
        .register("orch2", orch2)
        .build();

    // Extend it with builder_from
    let extended = OrchestrationRegistry::builder_from(&base)
        .register("orch3", orch3)
        .build();

    // Verify all three orchestrations are present
    let names = extended.list_orchestration_names();
    assert_eq!(names.len(), 3);
    assert!(names.contains(&"orch1".to_string()));
    assert!(names.contains(&"orch2".to_string()));
    assert!(names.contains(&"orch3".to_string()));

    // Verify base registry is unchanged
    let base_names = base.list_orchestration_names();
    assert_eq!(base_names.len(), 2);
}

#[test]
fn test_orchestration_registry_chained_register() {
    // register_all requires same function types, so we use chained .register() instead
    let registry = OrchestrationRegistry::builder()
        .register("orch1", orch1)
        .register("orch2", orch2)
        .register("orch3", orch3)
        .build();

    let names = registry.list_orchestration_names();
    assert_eq!(names.len(), 3);
    assert!(names.contains(&"orch1".to_string()));
    assert!(names.contains(&"orch2".to_string()));
    assert!(names.contains(&"orch3".to_string()));
}

#[test]
fn test_orchestration_registry_merge_with_chained_register() {
    let registry1 = OrchestrationRegistry::builder().register("orch1", orch1).build();

    let combined = OrchestrationRegistry::builder()
        .merge(registry1)
        .register("orch2", orch2)
        .register("orch3", orch3)
        .build();

    let names = combined.list_orchestration_names();
    assert_eq!(names.len(), 3);
}

#[test]
fn test_activity_registry_merge() {
    // Create first registry
    let registry1 = ActivityRegistry::builder()
        .register("activity1", activity1)
        .register("activity2", activity2)
        .build();

    // Create second registry
    let registry2 = ActivityRegistry::builder().register("activity3", activity3).build();

    // Merge both into a new registry
    let combined = ActivityRegistry::builder().merge(registry1).merge(registry2).build();

    // Verify all three activities are present
    assert!(combined.get("activity1").is_some());
    assert!(combined.get("activity2").is_some());
    assert!(combined.get("activity3").is_some());
}

#[test]
fn test_activity_registry_from_registry() {
    // Create base registry
    let base = ActivityRegistry::builder()
        .register("activity1", activity1)
        .register("activity2", activity2)
        .build();

    // Extend it with from_registry
    let extended = ActivityRegistryBuilder::from_registry(&base)
        .register("activity3", activity3)
        .build();

    // Verify all three activities are present
    assert!(extended.get("activity1").is_some());
    assert!(extended.get("activity2").is_some());
    assert!(extended.get("activity3").is_some());

    // Verify base registry is unchanged
    assert!(base.get("activity1").is_some());
    assert!(base.get("activity2").is_some());
    assert!(base.get("activity3").is_none());
}

#[test]
fn test_activity_registry_chained_register() {
    // register_all requires same function types, so we use chained .register() instead
    let registry = ActivityRegistry::builder()
        .register("activity1", activity1)
        .register("activity2", activity2)
        .register("activity3", activity3)
        .build();

    assert!(registry.get("activity1").is_some());
    assert!(registry.get("activity2").is_some());
    assert!(registry.get("activity3").is_some());
}

#[test]
fn test_activity_registry_merge_with_chained_register() {
    let registry1 = ActivityRegistry::builder().register("activity1", activity1).build();

    let combined = ActivityRegistry::builder()
        .merge(registry1)
        .register("activity2", activity2)
        .register("activity3", activity3)
        .build();

    assert!(combined.get("activity1").is_some());
    assert!(combined.get("activity2").is_some());
    assert!(combined.get("activity3").is_some());
}

#[tokio::test]
async fn test_register_versioned_typed() {
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize)]
    struct MyInput {
        value: i32,
    }

    #[derive(Serialize, Deserialize)]
    struct MyOutput {
        result: i32,
    }

    async fn typed_orch(_ctx: OrchestrationContext, input: MyInput) -> Result<MyOutput, String> {
        Ok(MyOutput {
            result: input.value * 2,
        })
    }

    let registry = OrchestrationRegistry::builder()
        .register_versioned_typed("typed-orch", "2.0.0", typed_orch)
        .build();

    // Verify it's registered
    let names = registry.list_orchestration_names();
    assert!(names.contains(&"typed-orch".to_string()));

    // Verify version
    let versions = registry.list_orchestration_versions("typed-orch");
    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0].to_string(), "2.0.0");
}

#[tokio::test]
async fn activity_context_metadata() {
    #[derive(Debug, PartialEq, Eq)]
    struct RecordedMetadata {
        instance_id: String,
        execution_id: u64,
        orchestration_name: String,
        orchestration_version: String,
        activity_name: String,
    }

    let recorded = Arc::new(Mutex::new(Vec::<RecordedMetadata>::new()));
    let recorded_for_activity = recorded.clone();

    let store = Arc::new(SqliteProvider::new_in_memory().await.unwrap());
    let activities = ActivityRegistry::builder()
        .register("Inspect", move |ctx: ActivityContext, _input: String| {
            let recorded_for_activity = recorded_for_activity.clone();
            async move {
                recorded_for_activity.lock().unwrap().push(RecordedMetadata {
                    instance_id: ctx.instance_id().to_string(),
                    execution_id: ctx.execution_id(),
                    orchestration_name: ctx.orchestration_name().to_string(),
                    orchestration_version: ctx.orchestration_version().to_string(),
                    activity_name: ctx.activity_name().to_string(),
                });
                Ok("ok".to_string())
            }
        })
        .build();

    let orchestrations = OrchestrationRegistry::builder()
        .register("InspectOrch", |ctx: OrchestrationContext, _input: String| async move {
            ctx.schedule_activity("Inspect", "payload").into_activity().await
        })
        .build();

    let rt = runtime::Runtime::start_with_store(store.clone(), Arc::new(activities), orchestrations).await;
    let client = Client::new(store.clone());
    client
        .start_orchestration("registry-test-instance", "InspectOrch", "")
        .await
        .unwrap();
    match client
        .wait_for_orchestration("registry-test-instance", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        OrchestrationStatus::Completed { output } => assert_eq!(output, "ok"),
        OrchestrationStatus::Failed { details } => {
            panic!("orchestration failed: {}", details.display_message())
        }
        other => panic!("unexpected orchestration status: {other:?}"),
    }

    rt.shutdown(None).await;

    let records = recorded.lock().unwrap();
    assert_eq!(records.len(), 1, "expected exactly one activity execution");
    let record = &records[0];
    assert_eq!(record.instance_id, "registry-test-instance");
    assert_eq!(record.execution_id, 1);
    assert_eq!(record.orchestration_name, "InspectOrch");
    assert_eq!(record.orchestration_version, "1.0.0");
    assert_eq!(record.activity_name, "Inspect");
}

#[tokio::test]
async fn test_cross_crate_composition_pattern() {
    // Simulate library crate 1
    fn create_azure_registry() -> OrchestrationRegistry {
        OrchestrationRegistry::builder()
            .register("duroxide-azure-arm::orchestration::provision-postgres", orch1)
            .register("duroxide-azure-arm::orchestration::deploy-webapp", orch2)
            .build()
    }

    // Simulate library crate 2
    fn create_aws_registry() -> OrchestrationRegistry {
        OrchestrationRegistry::builder()
            .register("duroxide-aws-ec2::orchestration::create-vpc", orch3)
            .build()
    }

    // Consumer code - compose both libraries
    let combined = OrchestrationRegistry::builder()
        .merge(create_azure_registry())
        .merge(create_aws_registry())
        .build();

    // Verify all orchestrations are present
    let names = combined.list_orchestration_names();
    assert_eq!(names.len(), 3);
    assert!(names.contains(&"duroxide-azure-arm::orchestration::provision-postgres".to_string()));
    assert!(names.contains(&"duroxide-azure-arm::orchestration::deploy-webapp".to_string()));
    assert!(names.contains(&"duroxide-aws-ec2::orchestration::create-vpc".to_string()));
}

// Introspection tests

#[test]
fn test_orchestration_registry_list_names() {
    let registry = OrchestrationRegistry::builder()
        .register("orch1", orch1)
        .register("orch2", orch2)
        .build();

    let names = registry.list_orchestration_names();
    assert_eq!(names.len(), 2);
    assert!(names.contains(&"orch1".to_string()));
    assert!(names.contains(&"orch2".to_string()));
}

#[test]
fn test_orchestration_registry_list_versions() {
    let registry = OrchestrationRegistry::builder()
        .register("orch1", orch1)
        .register_versioned("orch1", "2.0.0", orch2)
        .register_versioned("orch1", "3.0.0", orch3)
        .build();

    let versions = registry.list_orchestration_versions("orch1");
    assert_eq!(versions.len(), 3);
    assert!(versions.contains(&semver::Version::parse("1.0.0").unwrap()));
    assert!(versions.contains(&semver::Version::parse("2.0.0").unwrap()));
    assert!(versions.contains(&semver::Version::parse("3.0.0").unwrap()));

    // Non-existent orchestration returns empty vec
    let versions = registry.list_orchestration_versions("non-existent");
    assert_eq!(versions.len(), 0);
}

#[test]
fn test_activity_registry_list_names() {
    let registry = ActivityRegistry::builder()
        .register("activity1", activity1)
        .register("activity2", activity2)
        .register("activity3", activity3)
        .build();

    let names = registry.list_activity_names();
    assert_eq!(names.len(), 3);
    assert!(names.contains(&"activity1".to_string()));
    assert!(names.contains(&"activity2".to_string()));
    assert!(names.contains(&"activity3".to_string()));
}

#[test]
fn test_activity_registry_has() {
    let registry = ActivityRegistry::builder()
        .register("activity1", activity1)
        .register("activity2", activity2)
        .build();

    assert!(registry.has("activity1"));
    assert!(registry.has("activity2"));
    assert!(!registry.has("activity3"));
    assert!(!registry.has("non-existent"));
}

#[test]
fn test_activity_registry_count() {
    let empty = ActivityRegistry::builder().build();
    assert_eq!(empty.count(), 0);

    let registry = ActivityRegistry::builder()
        .register("activity1", activity1)
        .register("activity2", activity2)
        .register("activity3", activity3)
        .build();

    assert_eq!(registry.count(), 3);
}

#[test]
fn test_activity_registry_introspection_after_merge() {
    let registry1 = ActivityRegistry::builder()
        .register("lib1::activity1", activity1)
        .register("lib1::activity2", activity2)
        .build();

    let registry2 = ActivityRegistry::builder()
        .register("lib2::activity3", activity3)
        .build();

    let combined = ActivityRegistry::builder().merge(registry1).merge(registry2).build();

    // Test list_activity_names
    let names = combined.list_activity_names();
    assert_eq!(names.len(), 3);
    assert!(names.contains(&"lib1::activity1".to_string()));
    assert!(names.contains(&"lib1::activity2".to_string()));
    assert!(names.contains(&"lib2::activity3".to_string()));

    // Test has
    assert!(combined.has("lib1::activity1"));
    assert!(combined.has("lib2::activity3"));
    assert!(!combined.has("non-existent"));

    // Test count
    assert_eq!(combined.count(), 3);
}
