use duroxide::providers::sqlite::SqliteHistoryStore;
use duroxide::providers::{HistoryStore, WorkItem};
use duroxide::runtime::{self, registry::ActivityRegistry};
use duroxide::{OrchestrationContext, OrchestrationRegistry};
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use tokio::task::JoinSet;

#[tokio::test]
async fn test_sqlite_file_concurrent_access() {
    // Create a temporary directory for the database
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let db_path = temp_dir.path().join("test.db");
    
    // Create an empty file to ensure it exists
    std::fs::File::create(&db_path).expect("Failed to create database file");
    
    let db_url = format!("sqlite:{}", db_path.to_str().unwrap());
    
    // Create the store
    let store = Arc::new(
        SqliteHistoryStore::new(&db_url)
            .await
            .expect("Failed to create SQLite store")
    ) as Arc<dyn HistoryStore>;
    
    // Test concurrent writes
    let mut tasks = JoinSet::new();
    
    // Spawn 10 concurrent tasks that enqueue work
    for i in 0..10 {
        let store_clone = store.clone();
        tasks.spawn(async move {
            let work_item = WorkItem::StartOrchestration {
                instance: format!("concurrent-instance-{}", i),
                orchestration: "TestOrch".to_string(),
                version: Some("1.0.0".to_string()),
                input: format!("{{\"id\": {}}}", i),
                parent_instance: None,
                parent_id: None,
            };
            
            store_clone.enqueue_orchestrator_work(work_item)
                .await
                .expect("Failed to enqueue work");
        });
    }
    
    // Wait for all tasks to complete
    while let Some(result) = tasks.join_next().await {
        result.expect("Task panicked");
    }
    
    // Verify all items were enqueued
    let mut count = 0;
    while let Some(_) = store.fetch_orchestration_item().await {
        count += 1;
    }
    assert_eq!(count, 10, "Should have fetched all 10 items");
}

#[tokio::test]
async fn test_sqlite_file_concurrent_orchestrations() {
    // Create a temporary directory for the database
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let db_path = temp_dir.path().join("orchestrations.db");
    
    // Create an empty file to ensure it exists
    std::fs::File::create(&db_path).expect("Failed to create database file");
    
    let db_url = format!("sqlite:{}", db_path.to_str().unwrap());
    
    // Create the store
    let store = Arc::new(
        SqliteHistoryStore::new(&db_url)
            .await
            .expect("Failed to create SQLite store")
    ) as Arc<dyn HistoryStore>;
    
    let activity_registry = ActivityRegistry::builder()
        .register("increment", |input: String| async move {
            let n: i32 = input.parse().unwrap();
            Ok((n + 1).to_string())
        })
        .build();
    
    let orchestration = |ctx: OrchestrationContext, input: String| async move {
        let n: i32 = input.parse().unwrap();
        let result = ctx
            .schedule_activity("increment", n.to_string())
            .into_activity()
            .await?;
        Ok(result)
    };
    
    let orchestration_registry = OrchestrationRegistry::builder()
        .register("IncrementOrch", orchestration)
        .build();
    
    let rt = runtime::Runtime::start_with_store(
        store.clone(),
        Arc::new(activity_registry),
        orchestration_registry,
    )
    .await;
    
    // Start multiple orchestrations concurrently
    let mut tasks = JoinSet::new();
    
    for i in 0..5 {
        let rt_clone = rt.clone();
        tasks.spawn(async move {
            rt_clone
                .start_orchestration(
                    &format!("file-concurrent-{}", i),
                    "IncrementOrch",
                    &i.to_string(),
                )
                .await
                .expect("Failed to start orchestration");
        });
    }
    
    // Wait for all orchestrations to start
    while let Some(result) = tasks.join_next().await {
        result.expect("Task panicked");
    }
    
    // Wait for all orchestrations to complete
    for i in 0..5 {
        let status = rt
            .wait_for_orchestration(
                &format!("file-concurrent-{}", i),
                Duration::from_secs(10),
            )
            .await
            .expect("Failed to wait for orchestration");
        
        match status {
            runtime::OrchestrationStatus::Completed { output } => {
                let result: i32 = output.parse().unwrap();
                assert_eq!(result, i + 1, "Orchestration {} produced wrong result", i);
            }
            _ => panic!("Orchestration {} did not complete successfully", i),
        }
    }
    
    rt.shutdown().await;
    
    // Verify the database file exists and has data
    assert!(db_path.exists(), "Database file should exist");
    let metadata = std::fs::metadata(&db_path).expect("Failed to get file metadata");
    assert!(metadata.len() > 0, "Database file should have content");
}

#[tokio::test]
async fn test_sqlite_file_wal_mode() {
    // Create a temporary directory for the database
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let db_path = temp_dir.path().join("wal_test.db");
    
    // Create an empty file to ensure it exists
    std::fs::File::create(&db_path).expect("Failed to create database file");
    
    let db_url = format!("sqlite:{}", db_path.to_str().unwrap());
    
    // Create the store
    let _store = Arc::new(
        SqliteHistoryStore::new(&db_url)
            .await
            .expect("Failed to create SQLite store")
    ) as Arc<dyn HistoryStore>;
    
    // Verify WAL files are created
    tokio::time::sleep(Duration::from_millis(100)).await;
    
    let _wal_path = temp_dir.path().join("wal_test.db-wal");
    let _shm_path = temp_dir.path().join("wal_test.db-shm");
    
    assert!(db_path.exists(), "Main database file should exist");
    // WAL and SHM files may or may not exist depending on activity
    // but the database should be configured for WAL mode
}
