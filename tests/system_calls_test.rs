use duroxide::runtime::{self, registry::ActivityRegistry};
use duroxide::{OrchestrationContext, OrchestrationRegistry};
use std::sync::Arc;

#[tokio::test]
async fn test_new_guid() {
    let store = Arc::new(
        duroxide::providers::sqlite::SqliteProvider::new_in_memory()
            .await
            .unwrap(),
    );
    let activities = Arc::new(ActivityRegistry::builder().build());

    let orchestrations = OrchestrationRegistry::builder()
        .register("TestGuid", |ctx: OrchestrationContext, _input: String| async move {
            let guid1 = ctx.new_guid().await?;
            let guid2 = ctx.new_guid().await?;

            // GUIDs should be different
            assert_ne!(guid1, guid2);

            // GUIDs should be valid hex strings (excluding hyphens)
            assert!(guid1.chars().filter(|c| *c != '-').all(|c| c.is_ascii_hexdigit()));
            assert!(guid2.chars().filter(|c| *c != '-').all(|c| c.is_ascii_hexdigit()));

            Ok(format!("{},{}", guid1, guid2))
        })
        .build();

    let rt = runtime::Runtime::start_with_store(store.clone(), activities, orchestrations).await;
    let client = duroxide::Client::new(store.clone());
    client.start_orchestration("test-guid", "TestGuid", "").await.unwrap();
    let client = duroxide::Client::new(store.clone());
    let status = client
        .wait_for_orchestration("test-guid", tokio::time::Duration::from_secs(5))
        .await
        .unwrap();

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
    let store = Arc::new(
        duroxide::providers::sqlite::SqliteProvider::new_in_memory()
            .await
            .unwrap(),
    );
    let activities = Arc::new(ActivityRegistry::builder().build());

    let orchestrations = OrchestrationRegistry::builder()
        .register("TestTime", |ctx: OrchestrationContext, _input: String| async move {
            let time1 = ctx.utcnow_ms().await?;

            // Add a small timer to ensure time progresses
            ctx.schedule_timer(100).into_timer().await;

            let time2 = ctx.utcnow_ms().await?;

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
    let status = client
        .wait_for_orchestration("test-time", tokio::time::Duration::from_secs(5))
        .await
        .unwrap();

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
    let store = Arc::new(
        duroxide::providers::sqlite::SqliteProvider::new_in_memory()
            .await
            .unwrap(),
    );
    let activities = Arc::new(ActivityRegistry::builder().build());

    let orchestrations = OrchestrationRegistry::builder()
        .register(
            "TestDeterminism",
            |ctx: OrchestrationContext, _input: String| async move {
                let guid = ctx.new_guid().await?;
                let time = ctx.utcnow_ms().await?;

                // Use values in some computation
                let result = format!("guid:{},time:{}", guid, time);

                Ok(result)
            },
        )
        .build();

    let rt = runtime::Runtime::start_with_store(store.clone(), activities.clone(), orchestrations.clone()).await;

    // Run orchestration first time
    let instance = "test-determinism";
    let client = duroxide::Client::new(store.clone());
    client
        .start_orchestration(instance, "TestDeterminism", "")
        .await
        .unwrap();
    let client = duroxide::Client::new(store.clone());
    let status1 = client
        .wait_for_orchestration(instance, tokio::time::Duration::from_secs(5))
        .await
        .unwrap();

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
    let status2 = client2
        .wait_for_orchestration(instance, tokio::time::Duration::from_secs(5))
        .await
        .unwrap();

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
    let store = Arc::new(
        duroxide::providers::sqlite::SqliteProvider::new_in_memory()
            .await
            .unwrap(),
    );
    let activities = Arc::new(
        ActivityRegistry::builder()
            .register("QuickTask", |_: String| async move { Ok("task_done".to_string()) })
            .build(),
    );

    let orchestrations = OrchestrationRegistry::builder()
        .register("TestSelect", |ctx: OrchestrationContext, _input: String| async move {
            // Test: System calls should work correctly with activities in select/join

            // First, get a system call result
            let guid = ctx.new_guid().await?;

            // Test select2 with activities - system calls complete synchronously in the background
            let activity1 = ctx.schedule_activity("QuickTask", "task1");
            let activity2 = ctx.schedule_activity("QuickTask", "task2");

            let (winner_idx, output) = ctx.select2(activity1, activity2).await;

            let first_result = match output {
                duroxide::DurableOutput::Activity(Ok(s)) => s,
                _ => "unexpected".to_string(),
            };

            // Get another system call to verify they work throughout the orchestration
            let time = ctx.utcnow_ms().await?;

            // Verify both system calls returned valid values
            assert!(guid.len() == 36, "GUID should be valid");
            assert!(time > 0, "Time should be positive");

            Ok(format!(
                "winner:{},result:{},guid_len:{},time_valid:true",
                winner_idx,
                first_result,
                guid.len()
            ))
        })
        .build();

    let rt = runtime::Runtime::start_with_store(store.clone(), activities, orchestrations).await;
    let client = duroxide::Client::new(store.clone());
    client
        .start_orchestration("test-select", "TestSelect", "")
        .await
        .unwrap();
    let status = client
        .wait_for_orchestration("test-select", tokio::time::Duration::from_secs(5))
        .await
        .unwrap();

    if let duroxide::runtime::OrchestrationStatus::Completed { output } = status {
        println!("Output: {}", output);
        // Output should contain winner index, result, guid validation, and time validation
        assert!(output.starts_with("winner:"), "Output should start with 'winner:'");
        assert!(output.contains("result:task_done"), "Output should contain task result");
        assert!(output.contains("guid_len:36"), "GUID should be 36 chars");
        assert!(output.contains("time_valid:true"), "Time should be valid");
    } else {
        panic!("Orchestration did not complete successfully: {:?}", status);
    }

    rt.shutdown().await;
}

#[tokio::test]
async fn test_system_calls_join_with_activities() {
    let store = Arc::new(
        duroxide::providers::sqlite::SqliteProvider::new_in_memory()
            .await
            .unwrap(),
    );
    let activities = Arc::new(
        ActivityRegistry::builder()
            .register(
                "SlowTask",
                |input: String| async move { Ok(format!("processed:{}", input)) },
            )
            .build(),
    );

    let orchestrations = OrchestrationRegistry::builder()
        .register("TestJoin", |ctx: OrchestrationContext, _input: String| async move {
            // Test 1: Join system call futures with activity futures
            let guid_future = ctx.new_guid_future();
            let time_future = ctx.utcnow_ms_future();
            let activity_future = ctx.schedule_activity("SlowTask", "data1");

            let results = ctx.join(vec![guid_future, time_future, activity_future]).await;

            // All should complete successfully
            assert_eq!(results.len(), 3, "Should have 3 results from join");

            // Extract and validate results
            let guid = match &results[0] {
                duroxide::DurableOutput::Activity(Ok(s)) => s.clone(),
                _ => panic!("Expected GUID from first result"),
            };

            let time_str = match &results[1] {
                duroxide::DurableOutput::Activity(Ok(s)) => s.clone(),
                _ => panic!("Expected time from second result"),
            };

            let activity_result = match &results[2] {
                duroxide::DurableOutput::Activity(Ok(s)) => s.clone(),
                _ => panic!("Expected activity result from third result"),
            };

            // Validate the values
            assert_eq!(guid.len(), 36, "GUID should be 36 chars");
            let time: u64 = time_str.parse().expect("Time should parse");
            assert!(time > 0, "Time should be positive");
            assert_eq!(activity_result, "processed:data1");

            // Test 2: Select system call future vs activity future
            let guid_future2 = ctx.new_guid_future();
            let activity_future2 = ctx.schedule_activity("SlowTask", "data2");

            let (winner_idx, output) = ctx.select2(guid_future2, activity_future2).await;

            let winner_result = match output {
                duroxide::DurableOutput::Activity(Ok(s)) => s,
                _ => panic!("Expected activity output"),
            };

            // System call should typically win since it completes synchronously
            // But we accept either winner

            Ok(format!(
                "guid_len:{},time:{},activity:{},winner:{},winner_result:{}",
                guid.len(),
                time,
                activity_result,
                winner_idx,
                winner_result
            ))
        })
        .build();

    let rt = runtime::Runtime::start_with_store(store.clone(), activities, orchestrations).await;
    let client = duroxide::Client::new(store.clone());
    client.start_orchestration("test-join", "TestJoin", "").await.unwrap();
    let status = client
        .wait_for_orchestration("test-join", tokio::time::Duration::from_secs(5))
        .await
        .unwrap();

    if let duroxide::runtime::OrchestrationStatus::Completed { output } = status {
        println!("Output: {}", output);
        assert!(output.contains("guid_len:36"), "GUID should be 36 chars");
        assert!(
            output.contains("activity:processed:data1"),
            "Activity should process correctly"
        );
        assert!(output.contains("winner:"), "Should have winner");
    } else {
        panic!("Orchestration did not complete successfully: {:?}", status);
    }

    rt.shutdown().await;
}
