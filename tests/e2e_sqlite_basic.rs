use duroxide::providers::sqlite::SqliteHistoryStore;
use duroxide::providers::HistoryStore;
use duroxide::runtime::{self, registry::ActivityRegistry};
use duroxide::{OrchestrationContext, OrchestrationRegistry};
use std::sync::Arc;
use std::time::Duration;
use tempfile::NamedTempFile;

#[tokio::test]
async fn test_sqlite_provider_works() {
    // Create a temporary SQLite database
    let temp_file = NamedTempFile::new().expect("Failed to create temp file");
    let db_path = temp_file.path().to_str().unwrap();
    let db_url = format!("sqlite:{}", db_path);
    
    // Keep the file alive for the test
    std::mem::forget(temp_file);
    
    let store = Arc::new(
        SqliteHistoryStore::new(&db_url)
            .await
            .expect("Failed to create SQLite store")
    );

    // Register a simple activity
    let activity_registry = ActivityRegistry::builder()
        .register("double", |input: String| async move {
            let n: i32 = serde_json::from_str(&input).unwrap();
            Ok(serde_json::to_string(&(n * 2)).unwrap())
        })
        .build();

    // Register a simple orchestration
    let orchestration = |ctx: OrchestrationContext, input: String| async move {
        let n: i32 = serde_json::from_str(&input).unwrap();
        
        // Call activity twice
        let doubled = ctx
            .schedule_activity("double", serde_json::to_string(&n).unwrap())
            .into_activity()
            .await?;
        
        let quadrupled = ctx
            .schedule_activity("double", doubled.clone())
            .into_activity()
            .await?;
        
        Ok(quadrupled)
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("quadruple", orchestration)
        .build();

    // Start runtime with SQLite store
    let rt = runtime::Runtime::start_with_store(
        store.clone() as Arc<dyn HistoryStore>,
        Arc::new(activity_registry),
        orchestration_registry,
    )
    .await;

    // Start the orchestration
    rt.clone()
        .start_orchestration("test_sqlite_1", "quadruple", serde_json::to_string(&5).unwrap())
        .await
        .unwrap();

    // Wait for completion
    let status = rt.wait_for_orchestration("test_sqlite_1", Duration::from_secs(5))
        .await
        .unwrap();
    
    match status {
        runtime::OrchestrationStatus::Completed { output } => {
            let final_value: i32 = serde_json::from_str(&output).unwrap();
            assert_eq!(final_value, 20); // 5 * 2 * 2 = 20
        }
        runtime::OrchestrationStatus::Failed { error } => panic!("Orchestration failed: {}", error),
        _ => panic!("Unexpected status"),
    }
    
    // Verify we can read history back
    let history = store.read("test_sqlite_1").await;
    assert!(history.len() > 0);
    
    // Test restart scenario - start another instance
    rt.clone()
        .start_orchestration("test_sqlite_2", "quadruple", serde_json::to_string(&10).unwrap())
        .await
        .unwrap();
    
    let status2 = rt.wait_for_orchestration("test_sqlite_2", Duration::from_secs(5))
        .await
        .unwrap();
        
    match status2 {
        runtime::OrchestrationStatus::Completed { output } => {
            let final_value2: i32 = serde_json::from_str(&output).unwrap();
            assert_eq!(final_value2, 40); // 10 * 2 * 2 = 40
        }
        _ => panic!("Second orchestration failed"),
    }
}

#[tokio::test]
async fn test_sqlite_concurrent_instances() {
    // Create in-memory SQLite for speed
    let store = Arc::new(
        SqliteHistoryStore::new("sqlite::memory:")
            .await
            .expect("Failed to create SQLite store")
    );

    let activity_registry = ActivityRegistry::builder()
        .register("add_one", |input: String| async move {
            let n: i32 = serde_json::from_str(&input).unwrap();
            // Simulate some work
            tokio::time::sleep(Duration::from_millis(50)).await;
            Ok(serde_json::to_string(&(n + 1)).unwrap())
        })
        .build();

    let orchestration = |ctx: OrchestrationContext, input: String| async move {
        let n: i32 = serde_json::from_str(&input).unwrap();
        
        // Three sequential adds
        let mut result = serde_json::to_string(&n).unwrap();
        for _ in 0..3 {
            result = ctx
                .schedule_activity("add_one", result)
                .into_activity()
                .await?;
        }
        
        Ok(result)
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("add_three", orchestration)
        .build();

    let rt = runtime::Runtime::start_with_store(
        store.clone() as Arc<dyn HistoryStore>,
        Arc::new(activity_registry),
        orchestration_registry,
    )
    .await;

    // Start multiple instances concurrently
    let mut instance_ids = vec![];
    for i in 0..5 {
        let instance_id = format!("concurrent_{}", i);
        rt.clone()
            .start_orchestration(&instance_id, "add_three", serde_json::to_string(&(i * 10)).unwrap())
            .await
            .unwrap();
        instance_ids.push((i, instance_id));
    }

    // Wait for all to complete
    for (i, instance_id) in instance_ids {
        let status = rt.wait_for_orchestration(&instance_id, Duration::from_secs(10))
            .await
            .unwrap();
            
        match status {
            runtime::OrchestrationStatus::Completed { output } => {
                let final_value: i32 = serde_json::from_str(&output).unwrap();
                assert_eq!(final_value, (i * 10) + 3);
            }
            _ => panic!("Instance {} failed", instance_id),
        }
    }
}
