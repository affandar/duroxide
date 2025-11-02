use duroxide::providers::sqlite::SqliteProvider;
use duroxide::runtime::registry::{ActivityRegistry, OrchestrationRegistry};
use duroxide::runtime::Runtime;
use duroxide::{ActivityContext, Client, OrchestrationContext};
use sqlx::types::chrono::Utc;
use std::sync::Arc;

// Test orchestrations
async fn hello_orchestration(_ctx: OrchestrationContext, _input: String) -> Result<String, String> {
    Ok("Hello".to_string())
}

async fn goodbye_orchestration(_ctx: OrchestrationContext, _input: String) -> Result<String, String> {
    Ok("Goodbye".to_string())
}

async fn shared_orchestration(_ctx: OrchestrationContext, _input: String) -> Result<String, String> {
    Ok("Shared".to_string())
}

async fn shared_orchestration_v2(_ctx: OrchestrationContext, _input: String) -> Result<String, String> {
    Ok("Shared v2".to_string())
}

// Test activities
async fn greet_activity(_ctx: ActivityContext, _input: String) -> Result<String, String> {
    Ok("Greet".to_string())
}

async fn farewell_activity(_ctx: ActivityContext, _input: String) -> Result<String, String> {
    Ok("Farewell".to_string())
}

async fn shared_activity(_ctx: ActivityContext, _input: String) -> Result<String, String> {
    Ok("Shared".to_string())
}

#[tokio::test]
async fn test_registry_discovery() {
    // Create a SQLite provider
    let store = Arc::new(SqliteProvider::new("sqlite::memory:", None).await.unwrap());

    // Create registries with some orchestrations and activities
    let orch_registry = OrchestrationRegistry::builder()
        .register("HelloOrchestration", hello_orchestration)
        .register("GoodbyeOrchestration", goodbye_orchestration)
        .build();

    let activity_registry = Arc::new(
        ActivityRegistry::builder()
            .register("GreetActivity", greet_activity)
            .register("FarewellActivity", farewell_activity)
            .build(),
    );

    // Create runtime with SQLite provider (in-memory)
    let runtime = Runtime::start_with_store(store.clone(), activity_registry.clone(), orch_registry.clone()).await;

    // Get the provider from runtime
    let client = Client::new(store.clone());

    // Check if management capability is available
    assert!(
        client.has_management_capability(),
        "SqliteProvider should have management capability"
    );

    // Get registry snapshot
    let snapshot = client
        .get_registry_snapshot()
        .await
        .expect("Should be able to get registry snapshot");

    // Verify orchestrations were registered
    assert_eq!(snapshot.orchestrations.len(), 2, "Should have 2 orchestrations");
    
    let hello_orch = snapshot
        .orchestrations
        .iter()
        .find(|o| o.name == "HelloOrchestration")
        .expect("HelloOrchestration should be registered");
    
    assert_eq!(hello_orch.versions, vec!["1.0.0"], "HelloOrchestration should have version 1.0.0");
    // Check that registration timestamp is recent (within last minute)
    let now = Utc::now();
    let elapsed = now.signed_duration_since(hello_orch.registered_at);
    assert!(elapsed.num_seconds() < 60, "Registration timestamp should be recent");
    
    let goodbye_orch = snapshot
        .orchestrations
        .iter()
        .find(|o| o.name == "GoodbyeOrchestration")
        .expect("GoodbyeOrchestration should be registered");
    
    assert_eq!(goodbye_orch.versions, vec!["1.0.0"], "GoodbyeOrchestration should have version 1.0.0");
    
    // Verify activities were registered
    assert_eq!(snapshot.activities.len(), 2, "Should have 2 activities");
    
    let greet_activity = snapshot
        .activities
        .iter()
        .find(|a| a.name == "GreetActivity")
        .expect("GreetActivity should be registered");
    
    // Check that registration timestamp is recent (within last minute)
    let elapsed = now.signed_duration_since(greet_activity.registered_at);
    assert!(elapsed.num_seconds() < 60, "Registration timestamp should be recent");
    
    assert!(
        snapshot.activities.iter().any(|a| a.name == "FarewellActivity"),
        "FarewellActivity should be registered"
    );

    // Cleanup
    runtime.shutdown(None).await;
}

#[tokio::test]
async fn test_registry_deduplication_across_runtimes() {
    // This test verifies that when multiple runtimes register the same orchestrations/activities,
    // the provider deduplicates them (only one entry per name).
    
    let store = Arc::new(SqliteProvider::new("sqlite::memory:", None).await.unwrap());
    
    // First runtime with SharedOrchestration (v1.0.0 and v2.0.0), GoodbyeOrchestration, SharedActivity, FarewellActivity
    let orch_registry_1 = OrchestrationRegistry::builder()
        .register_versioned("SharedOrchestration", "1.0.0", shared_orchestration)
        .register_versioned("SharedOrchestration", "2.0.0", shared_orchestration_v2)
        .register("GoodbyeOrchestration", goodbye_orchestration)
        .build();

    let activity_registry_1 = Arc::new(
        ActivityRegistry::builder()
            .register("SharedActivity", shared_activity)
            .register("FarewellActivity", farewell_activity)
            .build(),
    );

    let runtime_1 = Runtime::start_with_store(
        store.clone(),
        activity_registry_1,
        orch_registry_1,
    )
    .await;
    
    // Get initial snapshot
    let client = Client::new(store.clone());
    let snapshot_1 = client
        .get_registry_snapshot()
        .await
        .expect("Failed to get first snapshot");
    
    assert_eq!(snapshot_1.orchestrations.len(), 2, "First runtime should register 2 orchestrations");
    assert_eq!(snapshot_1.activities.len(), 2, "First runtime should register 2 activities");
    
    let first_timestamp = snapshot_1
        .orchestrations
        .iter()
        .find(|o| o.name == "SharedOrchestration")
        .expect("SharedOrchestration should exist")
        .registered_at;
    
    // Small delay to ensure different timestamp
    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    
    // Second runtime with HelloOrchestration, SharedOrchestration (v1.0.0 only), GreetActivity, SharedActivity
    let orch_registry_2 = OrchestrationRegistry::builder()
        .register("HelloOrchestration", hello_orchestration)
        .register("SharedOrchestration", shared_orchestration)
        .build();

    let activity_registry_2 = Arc::new(
        ActivityRegistry::builder()
            .register("GreetActivity", greet_activity)
            .register("SharedActivity", shared_activity)
            .build(),
    );

    let runtime_2 = Runtime::start_with_store(
        store.clone(),
        activity_registry_2,
        orch_registry_2,
    )
    .await;
    
    // Get snapshot after second runtime starts
    let snapshot_2 = client
        .get_registry_snapshot()
        .await
        .expect("Failed to get second snapshot");
    
    // Should have 3 unique orchestrations (HelloOrchestration, SharedOrchestration, GoodbyeOrchestration)
    assert_eq!(
        snapshot_2.orchestrations.len(), 
        3, 
        "Should have 3 unique orchestrations (deduplicated)"
    );
    
    // Should have 3 unique activities (GreetActivity, SharedActivity, FarewellActivity)
    assert_eq!(
        snapshot_2.activities.len(), 
        3, 
        "Should have 3 unique activities (deduplicated)"
    );
    
    // Verify all expected orchestrations exist
    assert!(
        snapshot_2.orchestrations.iter().any(|o| o.name == "HelloOrchestration"),
        "HelloOrchestration should exist"
    );
    assert!(
        snapshot_2.orchestrations.iter().any(|o| o.name == "SharedOrchestration"),
        "SharedOrchestration should exist"
    );
    assert!(
        snapshot_2.orchestrations.iter().any(|o| o.name == "GoodbyeOrchestration"),
        "GoodbyeOrchestration should exist"
    );
    
    // Verify all expected activities exist
    assert!(
        snapshot_2.activities.iter().any(|a| a.name == "GreetActivity"),
        "GreetActivity should exist"
    );
    assert!(
        snapshot_2.activities.iter().any(|a| a.name == "SharedActivity"),
        "SharedActivity should exist"
    );
    assert!(
        snapshot_2.activities.iter().any(|a| a.name == "FarewellActivity"),
        "FarewellActivity should exist"
    );
    
    // Verify SharedOrchestration appears only ONCE (deduplicated)
    let shared_orch_count = snapshot_2
        .orchestrations
        .iter()
        .filter(|o| o.name == "SharedOrchestration")
        .count();
    assert_eq!(
        shared_orch_count, 
        1, 
        "SharedOrchestration should appear only once (deduplicated)"
    );
    
    // Verify SharedOrchestration has both versions (merged from both runtimes)
    let shared_orch = snapshot_2
        .orchestrations
        .iter()
        .find(|o| o.name == "SharedOrchestration")
        .expect("SharedOrchestration should exist");
    
    assert_eq!(
        shared_orch.versions.len(),
        2,
        "SharedOrchestration should have 2 versions"
    );
    assert!(
        shared_orch.versions.contains(&"1.0.0".to_string()),
        "SharedOrchestration should have version 1.0.0"
    );
    assert!(
        shared_orch.versions.contains(&"2.0.0".to_string()),
        "SharedOrchestration should have version 2.0.0"
    );
    
    // Verify SharedActivity appears only ONCE (deduplicated)
    let shared_activity_count = snapshot_2
        .activities
        .iter()
        .filter(|a| a.name == "SharedActivity")
        .count();
    assert_eq!(
        shared_activity_count, 
        1, 
        "SharedActivity should appear only once (deduplicated)"
    );
    
    // Verify timestamp was updated for shared items
    let second_timestamp = snapshot_2
        .orchestrations
        .iter()
        .find(|o| o.name == "SharedOrchestration")
        .expect("SharedOrchestration should exist")
        .registered_at;
    
    assert!(
        second_timestamp > first_timestamp,
        "SharedOrchestration timestamp should be updated when re-registered"
    );
    
    // Cleanup
    runtime_1.shutdown(None).await;
    runtime_2.shutdown(None).await;
}

#[tokio::test]
async fn test_empty_registry() {
    // Create a SQLite provider
    let store = Arc::new(SqliteProvider::new("sqlite::memory:", None).await.unwrap());

    // Create empty registries
    let orch_registry = OrchestrationRegistry::builder().build();
    let activity_registry = Arc::new(ActivityRegistry::builder().build());

    // Create runtime
    let runtime = Runtime::start_with_store(store.clone(), activity_registry, orch_registry).await;
    let client = Client::new(store.clone());

    // Get registry snapshot
    let snapshot = client
        .get_registry_snapshot()
        .await
        .expect("Should be able to get registry snapshot");

    // Verify empty
    assert_eq!(
        snapshot.orchestrations.len(),
        0,
        "Should have no orchestrations"
    );
    assert_eq!(snapshot.activities.len(), 0, "Should have no activities");

    // Cleanup
    runtime.shutdown(None).await;
}
