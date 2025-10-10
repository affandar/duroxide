// Quick diagnostic to check state saving
use duroxide::providers::sqlite::SqliteProvider;
use duroxide::runtime::registry::ActivityRegistry;
use duroxide::{Client, OrchestrationContext, OrchestrationRegistry};
use std::sync::Arc;

#[tokio::main]
async fn main() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.db");
    std::fs::File::create(&db_path).unwrap();
    let db_url = format!("sqlite:{}", db_path.to_str().unwrap());
    let store = Arc::new(SqliteProvider::new(&db_url).await.unwrap());
    
    let activities = ActivityRegistry::builder()
        .register("Task", |_: String| async move { Ok("result".to_string()) })
        .build();
    
    let orch = |ctx: OrchestrationContext, _: String| async move {
        ctx.schedule_activity("Task", "").into_activity().await
    };
    
    let orchestrations = OrchestrationRegistry::builder()
        .register("Test", orch)
        .build();
    
    let rt = duroxide::runtime::Runtime::start_with_store(
        store.clone(),
        Arc::new(activities),
        orchestrations
    ).await;
    
    let client = Client::new(store.clone());
    client.start_orchestration("test-1", "Test", "").await.unwrap();
    
    // Wait for completion
    let status = client.wait_for_orchestration("test-1", std::time::Duration::from_secs(5)).await.unwrap();
    println!("Status: {:?}", status);
    
    // Check database state directly
    let pool = store.get_pool();
    
    // Check executions table
    let exec_status: Option<String> = sqlx::query_scalar(
        "SELECT status FROM executions WHERE instance_id = 'test-1'"
    )
    .fetch_optional(pool)
    .await
    .unwrap();
    println!("Executions.status: {:?}", exec_status);
    
    // Check history events
    let history = store.read("test-1").await;
    println!("History events: {}", history.len());
    for event in &history {
        println!("  - {:?}", std::mem::discriminant(event));
    }
    
    // Check for terminal event
    let has_completed = history.iter().any(|e| matches!(e, duroxide::Event::OrchestrationCompleted { .. }));
    println!("Has OrchestrationCompleted event: {}", has_completed);
    
    rt.shutdown().await;
}
