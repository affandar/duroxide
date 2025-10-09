use duroxide::{
    runtime::{self, registry::ActivityRegistry, OrchestrationStatus},
    providers::Provider,
    OrchestrationContext, OrchestrationRegistry,
};
use std::sync::Arc;

#[tokio::test] 
async fn test_trace_deterministic_in_history() {
    let activities = ActivityRegistry::builder()
        .register("GetValue", |_: String| async move { Ok("test".to_string()) })
        .build();
    
    let orch = |ctx: OrchestrationContext, _: String| async move {
        ctx.trace_info("Test trace message");
        let result = ctx.schedule_activity("GetValue", "").into_activity().await?;
        ctx.trace_warn("Warning message");
        ctx.trace_error("Error message");
        Ok(result)
    };
    
    let history_store = Arc::new(duroxide::providers::sqlite::SqliteProvider::new_in_memory().await.unwrap());
    let orchestration_registry = OrchestrationRegistry::builder()
        .register("test_orch", orch)
        .build();
    let rt = runtime::Runtime::start_with_store(
        history_store.clone(), 
        Arc::new(activities), 
        orchestration_registry
    ).await;
    let client = duroxide::Client::new(history_store.clone());
    
    // Start orchestration
    client.start_orchestration("instance-2", "test_orch", "").await.unwrap();
    
    // Wait for completion
    let status = client.wait_for_orchestration("instance-2", std::time::Duration::from_millis(1000)).await.unwrap();
    assert!(matches!(status, OrchestrationStatus::Completed { .. }));
    
    // Check history contains trace system calls
    let history = history_store.read("instance-2").await;
    let trace_events: Vec<_> = history.iter()
        .filter(|e| matches!(e, duroxide::Event::SystemCall { op, .. } if op.starts_with("trace:")))
        .collect();
    
    assert_eq!(trace_events.len(), 3, "Should have exactly three trace events in history");
    
    // Now restart the runtime and re-run the same orchestration instance
    // This will force a replay of the history
    rt.shutdown().await;
    
    let rt2 = runtime::Runtime::start_with_store(
        history_store.clone(), 
        Arc::new(ActivityRegistry::builder()
            .register("GetValue", |_: String| async move { Ok("test".to_string()) })
            .build()), 
        OrchestrationRegistry::builder()
            .register("test_orch", orch)
            .build()
    ).await;
    
    // Wait a bit to ensure replay happens
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    
    // Check that no new trace events were added during replay
    let history_after_replay = history_store.read("instance-2").await;
    let trace_events_after: Vec<_> = history_after_replay.iter()
        .filter(|e| matches!(e, duroxide::Event::SystemCall { op, .. } if op.starts_with("trace:")))
        .collect();
    
    assert_eq!(trace_events_after.len(), 3, "No new trace events should be added during replay");
    
    rt2.shutdown().await;
}

#[tokio::test]

async fn test_trace_fire_and_forget() {
    let activities = ActivityRegistry::builder()
        .register("DoWork", |_: String| async move { 
            tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
            Ok("done".to_string()) 
        })
        .build();
    
    let orch = |ctx: OrchestrationContext, _: String| async move {
        // Traces should not block execution
        ctx.trace_info("Starting work");
        let future1 = ctx.schedule_activity("DoWork", "1");
        ctx.trace_info("Scheduled first activity");
        let future2 = ctx.schedule_activity("DoWork", "2");
        ctx.trace_info("Scheduled second activity");
        
        // Join both activities
        let results = ctx.join(vec![future1, future2]).await;
        ctx.trace_info(format!("Completed: {} activities", results.len()));
        
        Ok(format!("{} done", results.len()))
    };
    
    let history_store = Arc::new(duroxide::providers::sqlite::SqliteProvider::new_in_memory().await.unwrap());
    let orchestration_registry = OrchestrationRegistry::builder()
        .register("test_orch", orch)
        .build();
    let rt = runtime::Runtime::start_with_store(
        history_store.clone(), 
        Arc::new(activities), 
        orchestration_registry
    ).await;
    let client = duroxide::Client::new(history_store.clone());
    
    // Start orchestration
    client.start_orchestration("instance-3", "test_orch", "").await.unwrap();
    
    // Wait for completion
    let status = client.wait_for_orchestration("instance-3", std::time::Duration::from_millis(1000)).await.unwrap();
    
    match status {
        OrchestrationStatus::Completed { output } => assert_eq!(output, "2 done"),
        _ => panic!("Unexpected status: {:?}", status),
    }
    
    // Check that all traces were recorded
    let history = history_store.read("instance-3").await;
    let trace_events: Vec<_> = history.iter()
        .filter(|e| matches!(e, duroxide::Event::SystemCall { op, .. } if op.starts_with("trace:")))
        .collect();
    
    assert_eq!(trace_events.len(), 4, "Should have all trace events");
    
    rt.shutdown().await;
}
