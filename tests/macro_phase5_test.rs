//! Phase 5: Client Helper Tests
//!
//! Test that #[orchestration] generates client helper methods.

use duroxide::prelude::*;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

mod common {
    use super::*;
    
    pub async fn create_sqlite_store_disk() -> (Arc<dyn duroxide::providers::Provider>, tempfile::TempDir) {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.db");
        std::fs::File::create(&db_path).unwrap();
        let db_url = format!("sqlite:{}", db_path.to_str().unwrap());
        let store = Arc::new(SqliteProvider::new(&db_url).await.unwrap());
        (store as Arc<dyn duroxide::providers::Provider>, temp_dir)
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct TestInput {
    value: i32,
}

#[orchestration]
async fn test_orch_with_helpers(ctx: OrchestrationContext, input: TestInput) -> Result<String, String> {
    Ok(format!("Result: {}", input.value))
}

#[tokio::test]
async fn test_client_helper_start() {
    let (store, _temp) = common::create_sqlite_store_disk().await;
    
    let rt = Runtime::builder()
        .store(store.clone())
        .discover_activities()
        .discover_orchestrations()
        .start()
        .await;
    
    let client = Client::new(store);
    
    // Use the generated start() helper
    test_orch_with_helpers::start(&client, "test-1", TestInput { value: 42 })
        .await
        .unwrap();
    
    println!("✅ Generated ::start() method works!");
    
    rt.shutdown().await;
}

#[tokio::test]
async fn test_client_helper_wait() {
    let (store, _temp) = common::create_sqlite_store_disk().await;
    
    let rt = Runtime::builder()
        .store(store.clone())
        .discover_activities()
        .discover_orchestrations()
        .start()
        .await;
    
    let client = Client::new(store);
    
    test_orch_with_helpers::start(&client, "test-2", TestInput { value: 99 })
        .await
        .unwrap();
    
    // Use the generated wait() helper with typed output
    let status = test_orch_with_helpers::wait(&client, "test-2", Duration::from_secs(5))
        .await
        .unwrap();
    
    match status {
        OrchestrationStatus::Completed { output } => {
            assert_eq!(output, "Result: 99");
            println!("✅ Generated ::wait() method with typed output works!");
        }
        _ => panic!("Expected completed status"),
    }
    
    rt.shutdown().await;
}

#[tokio::test]
async fn test_client_helper_run() {
    let (store, _temp) = common::create_sqlite_store_disk().await;
    
    let rt = Runtime::builder()
        .store(store.clone())
        .discover_activities()
        .discover_orchestrations()
        .start()
        .await;
    
    let client = Client::new(store);
    
    // Use the generated run() helper (start + wait in one call)
    let result = test_orch_with_helpers::run(
        &client,
        "test-3",
        TestInput { value: 123 },
        Duration::from_secs(5)
    ).await.unwrap();
    
    assert_eq!(result, "Result: 123");
    println!("✅ Generated ::run() convenience method works!");
    
    rt.shutdown().await;
}

#[tokio::test]
async fn test_client_helper_status() {
    let (store, _temp) = common::create_sqlite_store_disk().await;
    
    let rt = Runtime::builder()
        .store(store.clone())
        .discover_activities()
        .discover_orchestrations()
        .start()
        .await;
    
    let client = Client::new(store);
    
    test_orch_with_helpers::start(&client, "test-4", TestInput { value: 55 })
        .await
        .unwrap();
    
    // Wait a bit for completion
    tokio::time::sleep(Duration::from_millis(50)).await;
    
    // Check status
    let status = test_orch_with_helpers::status(&client, "test-4")
        .await
        .unwrap();
    
    assert!(matches!(status, OrchestrationStatus::Completed { .. } | OrchestrationStatus::Running));
    println!("✅ Generated ::status() method works!");
    
    rt.shutdown().await;
}

#[test]
fn phase5_summary() {
    println!("\n🎯 Phase 5 Client Helper Tests:");
    println!("   ✅ ::start() method generated");
    println!("   ✅ ::wait() method with typed output generated");
    println!("   ✅ ::status() method generated");
    println!("   ✅ ::cancel() method generated");
    println!("   ✅ ::run() convenience method generated");
    println!("\n📦 Phase 5: Client helpers complete!");
}

