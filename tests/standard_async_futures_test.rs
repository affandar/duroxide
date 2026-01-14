//! Integration tests for the new standard async futures (v2).
//!
//! These tests validate that the v2 futures work end-to-end with:
//! - Schedule-time validation
//! - FIFO ordering
//! - Drop-based cancellation
//! - Compatibility with standard futures::select! and futures::join!

use duroxide::providers::sqlite::SqliteProvider;
use duroxide::runtime::registry::ActivityRegistry;
use duroxide::runtime::{OrchestrationStatus, Runtime};
use duroxide::{ActivityContext, Client, OrchestrationContext, OrchestrationRegistry};
use std::sync::Arc;
use std::time::Duration;

/// Helper to create a fresh in-memory SQLite provider for each test.
async fn create_test_provider() -> Arc<SqliteProvider> {
    Arc::new(
        SqliteProvider::new("sqlite::memory:", None)
            .await
            .expect("Failed to create provider"),
    )
}

/// Helper to extract output from OrchestrationStatus
fn extract_output(status: OrchestrationStatus) -> String {
    match status {
        OrchestrationStatus::Completed { output } => output,
        OrchestrationStatus::Failed { details } => {
            panic!("orchestration failed: {}", details.display_message())
        }
        other => panic!("unexpected orchestration status: {:?}", other),
    }
}

#[tokio::test]
async fn test_v2_activity_basic() {
    let store = create_test_provider().await;

    let activities = ActivityRegistry::builder()
        .register("Echo", |_ctx: ActivityContext, input: String| async move {
            Ok(format!("echoed: {}", input))
        })
        .build();

    let orchestration = |ctx: OrchestrationContext, input: String| async move {
        // Initialize v2 context
        ctx.initialize_v2();

        // Use v2 schedule method - direct await, no .into_activity()
        let result = ctx.schedule_activity_v2("Echo", &input).await?;
        Ok(result)
    };

    let orchestrations = OrchestrationRegistry::builder()
        .register("V2BasicTest", orchestration)
        .build();

    let rt = Runtime::start_with_store(store.clone(), Arc::new(activities), orchestrations).await;

    let client = Client::new(store.clone());
    client
        .start_orchestration("v2-basic-test", "V2BasicTest", "hello")
        .await
        .unwrap();

    let result = extract_output(
        client
            .wait_for_orchestration("v2-basic-test", Duration::from_secs(5))
            .await
            .unwrap(),
    );

    assert_eq!(result, "echoed: hello");

    rt.shutdown(None).await;
}

#[tokio::test]
async fn test_v2_timer_basic() {
    let store = create_test_provider().await;

    let activities = ActivityRegistry::builder().build();

    let orchestration = |ctx: OrchestrationContext, _input: String| async move {
        ctx.initialize_v2();

        // Use v2 timer - returns () directly
        ctx.schedule_timer_v2(Duration::from_millis(50)).await;

        Ok("timer_fired".to_string())
    };

    let orchestrations = OrchestrationRegistry::builder()
        .register("V2TimerTest", orchestration)
        .build();

    let rt = Runtime::start_with_store(store.clone(), Arc::new(activities), orchestrations).await;

    let client = Client::new(store.clone());
    client
        .start_orchestration("v2-timer-test", "V2TimerTest", "")
        .await
        .unwrap();

    let result = extract_output(
        client
            .wait_for_orchestration("v2-timer-test", Duration::from_secs(5))
            .await
            .unwrap(),
    );

    assert_eq!(result, "timer_fired");

    rt.shutdown(None).await;
}

#[tokio::test]
async fn test_v2_external_event_basic() {
    let store = create_test_provider().await;

    let activities = ActivityRegistry::builder().build();

    let orchestration = |ctx: OrchestrationContext, _input: String| async move {
        ctx.initialize_v2();

        // Wait for external event - returns String directly
        let payload = ctx.schedule_wait_v2("Approval").await;

        Ok(format!("approved: {}", payload))
    };

    let orchestrations = OrchestrationRegistry::builder()
        .register("V2ExternalTest", orchestration)
        .build();

    let rt = Runtime::start_with_store(store.clone(), Arc::new(activities), orchestrations).await;

    let client = Client::new(store.clone());
    client
        .start_orchestration("v2-external-test", "V2ExternalTest", "")
        .await
        .unwrap();

    // Wait a bit for orchestration to start
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Raise the external event
    client
        .raise_event("v2-external-test", "Approval", "yes")
        .await
        .unwrap();

    let result = extract_output(
        client
            .wait_for_orchestration("v2-external-test", Duration::from_secs(5))
            .await
            .unwrap(),
    );

    assert_eq!(result, "approved: yes");

    rt.shutdown(None).await;
}

#[tokio::test]
async fn test_v2_activity_sequence() {
    let store = create_test_provider().await;

    let activities = ActivityRegistry::builder()
        .register("Step", |_ctx: ActivityContext, input: String| async move {
            Ok(format!("done:{}", input))
        })
        .build();

    let orchestration = |ctx: OrchestrationContext, _input: String| async move {
        ctx.initialize_v2();

        // Multiple sequential activities
        let r1 = ctx.schedule_activity_v2("Step", "1").await?;
        let r2 = ctx.schedule_activity_v2("Step", "2").await?;
        let r3 = ctx.schedule_activity_v2("Step", "3").await?;

        Ok(format!("{},{},{}", r1, r2, r3))
    };

    let orchestrations = OrchestrationRegistry::builder()
        .register("V2SequenceTest", orchestration)
        .build();

    let rt = Runtime::start_with_store(store.clone(), Arc::new(activities), orchestrations).await;

    let client = Client::new(store.clone());
    client
        .start_orchestration("v2-seq-test", "V2SequenceTest", "")
        .await
        .unwrap();

    let result = extract_output(
        client
            .wait_for_orchestration("v2-seq-test", Duration::from_secs(5))
            .await
            .unwrap(),
    );

    assert_eq!(result, "done:1,done:2,done:3");

    rt.shutdown(None).await;
}

#[tokio::test]
async fn test_v2_activity_failure() {
    let store = create_test_provider().await;

    let activities = ActivityRegistry::builder()
        .register("Fail", |_ctx: ActivityContext, _input: String| async move {
            Err::<String, String>("intentional failure".to_string())
        })
        .build();

    let orchestration = |ctx: OrchestrationContext, _input: String| async move {
        ctx.initialize_v2();

        // This should return Err
        let result = ctx.schedule_activity_v2("Fail", "").await;

        match result {
            Ok(_) => Ok("unexpected_success".to_string()),
            Err(e) => Ok(format!("caught: {}", e)),
        }
    };

    let orchestrations = OrchestrationRegistry::builder()
        .register("V2FailTest", orchestration)
        .build();

    let rt = Runtime::start_with_store(store.clone(), Arc::new(activities), orchestrations).await;

    let client = Client::new(store.clone());
    client
        .start_orchestration("v2-fail-test", "V2FailTest", "")
        .await
        .unwrap();

    let result = extract_output(
        client
            .wait_for_orchestration("v2-fail-test", Duration::from_secs(5))
            .await
            .unwrap(),
    );

    assert_eq!(result, "caught: intentional failure");

    rt.shutdown(None).await;
}

#[tokio::test]
async fn test_v2_select_with_futures_crate() {
    use futures::select;
    use std::pin::pin;

    let store = create_test_provider().await;

    let activities = ActivityRegistry::builder()
        .register("Slow", |_ctx: ActivityContext, _input: String| async move {
            tokio::time::sleep(Duration::from_secs(10)).await;
            Ok("slow_done".to_string())
        })
        .build();

    let orchestration = |ctx: OrchestrationContext, _input: String| async move {
        ctx.initialize_v2();

        // Use futures::select! with v2 futures
        let mut activity = pin!(ctx.schedule_activity_v2("Slow", ""));
        let mut timer = pin!(ctx.schedule_timer_v2(Duration::from_millis(100)));

        let result = loop {
            select! {
                res = activity => break format!("activity: {:?}", res),
                _ = timer => break "timeout".to_string(),
            }
        };

        Ok(result)
    };

    let orchestrations = OrchestrationRegistry::builder()
        .register("V2SelectTest", orchestration)
        .build();

    let rt = Runtime::start_with_store(store.clone(), Arc::new(activities), orchestrations).await;

    let client = Client::new(store.clone());
    client
        .start_orchestration("v2-select-test", "V2SelectTest", "")
        .await
        .unwrap();

    let result = extract_output(
        client
            .wait_for_orchestration("v2-select-test", Duration::from_secs(5))
            .await
            .unwrap(),
    );

    assert_eq!(result, "timeout");

    rt.shutdown(None).await;
}

#[tokio::test]
async fn test_v2_join_with_futures_crate() {
    use futures::join;

    let store = create_test_provider().await;

    let activities = ActivityRegistry::builder()
        .register("Task", |_ctx: ActivityContext, input: String| async move {
            Ok(format!("result:{}", input))
        })
        .build();

    let orchestration = |ctx: OrchestrationContext, _input: String| async move {
        ctx.initialize_v2();

        // Use futures::join! with v2 futures
        let f1 = ctx.schedule_activity_v2("Task", "A");
        let f2 = ctx.schedule_activity_v2("Task", "B");
        let f3 = ctx.schedule_activity_v2("Task", "C");

        let (r1, r2, r3) = join!(f1, f2, f3);

        Ok(format!("{},{},{}", r1?, r2?, r3?))
    };

    let orchestrations = OrchestrationRegistry::builder()
        .register("V2JoinTest", orchestration)
        .build();

    let rt = Runtime::start_with_store(store.clone(), Arc::new(activities), orchestrations).await;

    let client = Client::new(store.clone());
    client
        .start_orchestration("v2-join-test", "V2JoinTest", "")
        .await
        .unwrap();

    let result = extract_output(
        client
            .wait_for_orchestration("v2-join-test", Duration::from_secs(5))
            .await
            .unwrap(),
    );

    assert_eq!(result, "result:A,result:B,result:C");

    rt.shutdown(None).await;
}

#[tokio::test]
async fn test_v2_replay_determinism() {
    // This test validates that the v2 system replays correctly
    // by running an orchestration that schedules multiple operations
    // and verifying the final result is consistent

    let store = create_test_provider().await;

    let activities = ActivityRegistry::builder()
        .register("GetValue", |_ctx: ActivityContext, key: String| async move {
            // Return deterministic value based on key
            Ok(format!("value_{}", key))
        })
        .build();

    let orchestration = |ctx: OrchestrationContext, _input: String| async move {
        ctx.initialize_v2();

        // Mix of activities and timers
        let v1 = ctx.schedule_activity_v2("GetValue", "key1").await?;
        ctx.schedule_timer_v2(Duration::from_millis(10)).await;
        let v2 = ctx.schedule_activity_v2("GetValue", "key2").await?;

        Ok(format!("{}+{}", v1, v2))
    };

    let orchestrations = OrchestrationRegistry::builder()
        .register("V2ReplayTest", orchestration)
        .build();

    let rt = Runtime::start_with_store(store.clone(), Arc::new(activities), orchestrations).await;

    let client = Client::new(store.clone());
    client
        .start_orchestration("v2-replay-test", "V2ReplayTest", "")
        .await
        .unwrap();

    let result = extract_output(
        client
            .wait_for_orchestration("v2-replay-test", Duration::from_secs(5))
            .await
            .unwrap(),
    );

    assert_eq!(result, "value_key1+value_key2");

    rt.shutdown(None).await;
}
