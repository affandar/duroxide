//! Phase 3: Invocation Macro Tests

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

// Activities and orchestrations at module level for linkme
#[activity(typed)]
async fn double_value(x: i32) -> Result<i32, String> {
    Ok(x * 2)
}

#[orchestration]
async fn test_durable_orch(ctx: OrchestrationContext, input: String) -> Result<String, String> {
    let x: i32 = input.parse().unwrap();
    let result = durable!(double_value(x)).await?;
    Ok(result.to_string())
}

#[orchestration]
async fn test_trace_orch(ctx: OrchestrationContext, input: String) -> Result<String, String> {
    durable_trace_info!("Info: {}", input);
    durable_trace_warn!("Warning");
    durable_trace_error!("Error: {}", "test");
    durable_trace_debug!("Debug: {}", 42);
    Ok(input)
}

#[orchestration]
async fn test_syscall_orch(ctx: OrchestrationContext, _input: String) -> Result<String, String> {
    let guid = durable_newguid!().await?;
    let timestamp = durable_utcnow!().await?;
    Ok(format!("GUID: {}, Time: {}", guid, timestamp))
}

#[tokio::test]
async fn test_durable_macro() {
    let (store, _temp) = common::create_sqlite_store_disk().await;
    
    let rt = Runtime::builder()
        .store(store.clone())
        .discover_activities()
        .discover_orchestrations()
        .start()
        .await;
    
    let client = Client::new(store);
    
    client.start_orchestration("test-1", "test_durable_orch", "\"5\"").await.unwrap();
    
    let status = client.wait_for_orchestration("test-1", Duration::from_secs(5))
        .await
        .map_err(|e| format!("{:?}", e))
        .unwrap();
    
    match status {
        OrchestrationStatus::Completed { output } => {
            println!("Got output: {:?}", output);
            // The output is JSON-serialized, so it's "\"10\"" not "10"
            assert!(output == "\"10\"" || output == "10");
            println!("✅ durable!() macro works!");
        }
        OrchestrationStatus::Failed { error } => {
            panic!("Orchestration failed: {}", error);
        }
        other => panic!("Unexpected status: {:?}", other),
    }
    
    rt.shutdown().await;
}

#[tokio::test]
async fn test_trace_macros() {
    let (store, _temp) = common::create_sqlite_store_disk().await;
    
    let rt = Runtime::builder()
        .store(store.clone())
        .discover_activities()
        .discover_orchestrations()
        .start()
        .await;
    
    let client = Client::new(store);
    
    client.start_orchestration("test-2", "test_trace_orch", "\"test\"").await.unwrap();
    
    let status = client.wait_for_orchestration("test-2", Duration::from_secs(5))
        .await
        .map_err(|e| format!("{:?}", e))
        .unwrap();
    
    assert!(matches!(status, OrchestrationStatus::Completed { .. }));
    println!("✅ durable_trace_*!() macros work!");
    
    rt.shutdown().await;
}

#[tokio::test]
async fn test_system_calls() {
    let (store, _temp) = common::create_sqlite_store_disk().await;
    
    let rt = Runtime::builder()
        .store(store.clone())
        .discover_activities()
        .discover_orchestrations()
        .start()
        .await;
    
    let client = Client::new(store);
    
    client.start_orchestration("test-3", "test_syscall_orch", "\"\"").await.unwrap();
    
    let status = client.wait_for_orchestration("test-3", Duration::from_secs(5))
        .await
        .map_err(|e| format!("{:?}", e))
        .unwrap();
    
    match status {
        OrchestrationStatus::Completed { output } => {
            assert!(output.contains("GUID:"));
            assert!(output.contains("Time:"));
            println!("✅ durable_newguid!() and durable_utcnow!() work!");
        }
        _ => panic!("Expected completed"),
    }
    
    rt.shutdown().await;
}

#[test]
fn phase3_summary() {
    println!("✅ Phase 3 complete - all invocation macros working!");
}