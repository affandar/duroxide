use duroxide::{OrchestrationContext, OrchestrationRegistry};
use duroxide::runtime::{self, registry::ActivityRegistry};
use std::sync::Arc;

#[tokio::test]
async fn test_new_guid() {
    let store = Arc::new(duroxide::providers::sqlite::SqliteProvider::new_in_memory().await.unwrap());
    let activities = Arc::new(ActivityRegistry::builder().build());
    
    let orchestrations = OrchestrationRegistry::builder()
        .register("TestGuid", |ctx: OrchestrationContext, _input: String| async move {
            let guid1 = ctx.new_guid().into_activity().await.unwrap();
            let guid2 = ctx.new_guid().into_activity().await.unwrap();
            
            // GUIDs should be different
            assert_ne!(guid1, guid2);
            
            // GUIDs should be valid hex strings
            assert!(guid1.chars().all(|c| c.is_ascii_hexdigit()));
            assert!(guid2.chars().all(|c| c.is_ascii_hexdigit()));
            
            Ok(format!("{},{}", guid1, guid2))
        })
        .build();
        
    let rt = runtime::Runtime::start_with_store(store.clone(), activities, orchestrations).await;
    let client = duroxide::Client::new(store.clone());
    client.start_orchestration("test-guid", "TestGuid", "").await.unwrap();
    let client = duroxide::Client::new(store.clone());
    let status = client.wait_for_orchestration("test-guid", tokio::time::Duration::from_secs(5)).await.unwrap();
    
    if let duroxide::runtime::OrchestrationStatus::Completed { output } = status {
        // Result should contain two different GUIDs
        let parts: Vec<&str> = output.split(',').collect();
        assert_eq!(parts.len(), 2);
        assert_ne!(parts[0], parts[1]);
    } else {
        panic!("Orchestration did not complete successfully: {:?}", status);
    }
    
    rt.shutdown().await;
}

#[tokio::test]
async fn test_utcnow_ms() {
    let store = Arc::new(duroxide::providers::sqlite::SqliteProvider::new_in_memory().await.unwrap());
    let activities = Arc::new(ActivityRegistry::builder().build());
    
    let orchestrations = OrchestrationRegistry::builder()
        .register("TestTime", |ctx: OrchestrationContext, _input: String| async move {
            let time1: u128 = ctx.utcnow_ms().into_activity().await.unwrap().parse().unwrap();
            
            // Add a small timer to ensure time progresses
            ctx.schedule_timer(100).into_timer().await;
            
            let time2: u128 = ctx.utcnow_ms().into_activity().await.unwrap().parse().unwrap();
            
            // Times should be valid millisecond timestamps
            let t1 = time1;
            let t2 = time2;
            
            // Times should be reasonable (after year 2020)
            assert!(t1 > 1577836800000); // Jan 1, 2020
            assert!(t2 > 1577836800000);
            
            // Second time should be after first (since we had a timer in between)
            assert!(t2 >= t1);
            
            Ok(format!("{},{}", time1, time2))
        })
        .build();
        
    let rt = runtime::Runtime::start_with_store(store.clone(), activities, orchestrations).await;
    let client = duroxide::Client::new(store.clone());
    client.start_orchestration("test-time", "TestTime", "").await.unwrap();
    let client = duroxide::Client::new(store.clone());
    let status = client.wait_for_orchestration("test-time", tokio::time::Duration::from_secs(5)).await.unwrap();
    
    if let duroxide::runtime::OrchestrationStatus::Completed { output } = status {
        // Result should contain two timestamps
        let parts: Vec<&str> = output.split(',').collect();
        assert_eq!(parts.len(), 2);
    } else {
        panic!("Orchestration did not complete successfully: {:?}", status);
    }
    
    rt.shutdown().await;
}

#[tokio::test]
async fn test_system_calls_deterministic_replay() {
    let store = Arc::new(duroxide::providers::sqlite::SqliteProvider::new_in_memory().await.unwrap());
    let activities = Arc::new(ActivityRegistry::builder().build());
    
    let orchestrations = OrchestrationRegistry::builder()
        .register("TestDeterminism", |ctx: OrchestrationContext, _input: String| async move {
            let guid = ctx.new_guid().into_activity().await.unwrap();
            let time = ctx.utcnow_ms().into_activity().await.unwrap();
            
            // Use values in some computation
            let result = format!("guid:{},time:{}", guid, time);
            
            Ok(result)
        })
        .build();
        
    let rt = runtime::Runtime::start_with_store(store.clone(), activities.clone(), orchestrations.clone()).await;
    
    // Run orchestration first time
    let instance = "test-determinism";
    let client = duroxide::Client::new(store.clone());
    client.start_orchestration(instance, "TestDeterminism", "").await.unwrap();
    let client = duroxide::Client::new(store.clone());
    let status1 = client.wait_for_orchestration(instance, tokio::time::Duration::from_secs(5)).await.unwrap();
    
    let output1 = if let duroxide::runtime::OrchestrationStatus::Completed { output } = status1 {
        output
    } else {
        panic!("First run did not complete successfully: {:?}", status1);
    };
    
    rt.shutdown().await;
    
    // Start new runtime with same store (simulating restart)
    let rt2 = runtime::Runtime::start_with_store(store.clone(), activities, orchestrations).await;
    
    // The orchestration should complete with the same result due to deterministic replay
    let client2 = duroxide::Client::new(store.clone());
    let status2 = client2.wait_for_orchestration(instance, tokio::time::Duration::from_secs(5)).await.unwrap();
    
    let output2 = if let duroxide::runtime::OrchestrationStatus::Completed { output } = status2 {
        output
    } else {
        panic!("Second run did not complete successfully: {:?}", status2);
    };
    
    // Outputs should be identical
    assert_eq!(output1, output2);
    
    rt2.shutdown().await;
}

#[tokio::test]
async fn test_system_calls_with_select() {
    let store = Arc::new(duroxide::providers::sqlite::SqliteProvider::new_in_memory().await.unwrap());
    let activities = Arc::new(ActivityRegistry::builder()
        .register("QuickTask", |_: String| async move {
            Ok("task_done".to_string())
        })
        .build());
    
    let orchestrations = OrchestrationRegistry::builder()
        .register("TestSelect", |ctx: OrchestrationContext, _input: String| async move {
            // System calls should complete immediately, even with select
            // First test that GUID completes immediately
            let guid = ctx.new_guid().into_activity().await.unwrap();
            
            // Now test racing against an activity
            let guid_future = ctx.new_guid();
            let activity_future = ctx.schedule_activity("QuickTask", "");
            
            let (winner_idx, output) = ctx.select2(guid_future, activity_future).await;
            
            match winner_idx {
                0 => {
                    // GUID won
                    if let duroxide::DurableOutput::Activity(Ok(guid2)) = output {
                        Ok(format!("guid_won:{},{}", guid, guid2))
                    } else {
                        Err("Unexpected output type".to_string())
                    }
                }
                1 => {
                    // Activity won
                    if let duroxide::DurableOutput::Activity(Ok(result)) = output {
                        Ok(format!("activity_won:{},{}", guid, result))
                    } else {
                        Err("Unexpected output type".to_string())
                    }
                }
                _ => Err("Invalid winner index".to_string())
            }
        })
        .build();
        
    let rt = runtime::Runtime::start_with_store(store.clone(), activities, orchestrations).await;
    let client = duroxide::Client::new(store.clone());
    client.start_orchestration("test-select", "TestSelect", "").await.unwrap();
    let status = client.wait_for_orchestration("test-select", tokio::time::Duration::from_secs(5)).await.unwrap();
    
    if let duroxide::runtime::OrchestrationStatus::Completed { output } = status {
        println!("Output: {}", output);
        // Parse the output
        if output.starts_with("guid_won:") {
            let guid_part = output.strip_prefix("guid_won:").unwrap();
            let parts: Vec<&str> = guid_part.split(',').collect();
            assert_eq!(parts.len(), 2, "Expected two GUIDs");
            assert!(parts[0].chars().all(|c| c.is_ascii_hexdigit()), "First GUID should be hex");
            assert!(parts[1].chars().all(|c| c.is_ascii_hexdigit()), "Second GUID should be hex");
        } else if output.starts_with("activity_won:") {
            let activity_part = output.strip_prefix("activity_won:").unwrap();
            let parts: Vec<&str> = activity_part.split(',').collect();
            assert_eq!(parts.len(), 2, "Expected GUID and activity result");
            assert!(parts[0].chars().all(|c| c.is_ascii_hexdigit()), "First part should be GUID");
            assert_eq!(parts[1], "task_done", "Second part should be activity result");
        } else {
            panic!("Unexpected output format: {}", output);
        }
        
        // Test passed - either guid_won or activity_won is acceptable
        // The key is that system calls work correctly
    } else {
        panic!("Orchestration did not complete successfully: {:?}", status);
    }
    
    rt.shutdown().await;
}
