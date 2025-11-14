use std::sync::Arc;
use std::time::Duration;

mod common;
use duroxide::runtime::registry::ActivityRegistry;
use duroxide::runtime::{self, RuntimeOptions};
use duroxide::{Client, OrchestrationContext, OrchestrationRegistry};

/// Test that default lock timeouts are set correctly
#[tokio::test]
async fn default_lock_timeouts() {
    let options = RuntimeOptions::default();
    assert_eq!(
        options.orchestrator_lock_timeout_secs, 5,
        "Default orchestrator lock timeout should be 5 seconds"
    );
    assert_eq!(
        options.worker_lock_timeout_secs, 30,
        "Default worker lock timeout should be 30 seconds"
    );
}

/// Test that custom lock timeouts can be configured via RuntimeOptions
#[tokio::test]
async fn custom_lock_timeout_configuration() {
    let options = RuntimeOptions {
        orchestrator_lock_timeout_secs: 10,
        worker_lock_timeout_secs: 120,
        ..Default::default()
    };
    assert_eq!(options.orchestrator_lock_timeout_secs, 10);
    assert_eq!(options.worker_lock_timeout_secs, 120);

    let short_options = RuntimeOptions {
        orchestrator_lock_timeout_secs: 1,
        worker_lock_timeout_secs: 5,
        ..Default::default()
    };
    assert_eq!(short_options.orchestrator_lock_timeout_secs, 1);
    assert_eq!(short_options.worker_lock_timeout_secs, 5);
}

/// Test that long-running activity gets retried after lock timeout expires
/// This test verifies end-to-end behavior: activity takes longer than lock timeout,
/// lock expires, activity is retried, and eventually completes.
#[tokio::test]
async fn long_running_activity_retries_after_timeout() {
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;

    // Track how many times activity is called
    let call_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let call_count_clone = call_count.clone();

    let orch = |ctx: OrchestrationContext, _input: String| async move {
        let result = ctx.schedule_activity("LongActivity", "data").into_activity().await?;
        Ok(result)
    };

    let acts = ActivityRegistry::builder()
        .register("LongActivity", move |_ctx: duroxide::ActivityContext, input: String| {
            let count = call_count_clone.clone();
            async move {
                let attempt = count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);

                if attempt == 0 {
                    // First attempt: simulate stuck activity (sleep longer than lock timeout)
                    tokio::time::sleep(Duration::from_millis(3000)).await;
                    // This will timeout and be retried
                    Ok(format!("attempt-{}: {}", attempt, input))
                } else {
                    // Second attempt: complete quickly
                    Ok(format!("attempt-{}: {}", attempt, input))
                }
            }
        })
        .build();

    let reg = OrchestrationRegistry::builder().register("TestOrch", orch).build();

    // Use 2 second worker lock timeout (shorter than first activity attempt)
    let options = RuntimeOptions {
        worker_lock_timeout_secs: 2,
        ..Default::default()
    };

    let rt = runtime::Runtime::start_with_options(store.clone(), Arc::new(acts), reg, options).await;
    let client = Client::new(store.clone());

    let inst = "inst-long-activity";
    client.start_orchestration(inst, "TestOrch", "").await.unwrap();

    // Wait for orchestration to complete (should retry after timeout)
    let status = client
        .wait_for_orchestration(inst, Duration::from_secs(10))
        .await
        .unwrap();

    match status {
        runtime::OrchestrationStatus::Completed { output } => {
            // Should have been called at least twice (initial + retry after timeout)
            let attempts = call_count.load(std::sync::atomic::Ordering::SeqCst);
            assert!(
                attempts >= 2,
                "Activity should have been retried after lock timeout, got {} attempts",
                attempts
            );
            assert!(output.contains("attempt-"), "Output should show attempt number");
        }
        runtime::OrchestrationStatus::Failed { details } => {
            panic!("Orchestration failed: {}", details.display_message());
        }
        _ => panic!("Unexpected orchestration status"),
    }

    rt.shutdown(None).await;
}

/// Test that orchestration with custom timeout completes successfully
#[tokio::test]
async fn orchestration_with_custom_timeout_completes() {
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;

    let orch = |ctx: OrchestrationContext, _input: String| async move {
        let result = ctx.schedule_activity("TestActivity", "data").into_activity().await?;
        Ok(result)
    };

    let acts = ActivityRegistry::builder()
        .register(
            "TestActivity",
            |_ctx: duroxide::ActivityContext, input: String| async move { Ok(format!("processed: {}", input)) },
        )
        .build();

    let reg = OrchestrationRegistry::builder().register("TestOrch", orch).build();

    // Use custom lock timeouts (60 seconds - plenty of time)
    let options = RuntimeOptions {
        orchestrator_lock_timeout_secs: 60,
        worker_lock_timeout_secs: 60,
        ..Default::default()
    };

    let rt = runtime::Runtime::start_with_options(store.clone(), Arc::new(acts), reg, options).await;
    let client = Client::new(store.clone());

    let inst = "inst-custom-timeout";
    client.start_orchestration(inst, "TestOrch", "").await.unwrap();

    // Should complete successfully with custom timeout
    let status = client
        .wait_for_orchestration(inst, Duration::from_secs(5))
        .await
        .unwrap();

    match status {
        runtime::OrchestrationStatus::Completed { output } => {
            assert_eq!(output, "processed: data");
        }
        runtime::OrchestrationStatus::Failed { details } => {
            panic!("Orchestration failed: {}", details.display_message());
        }
        _ => panic!("Unexpected orchestration status"),
    }

    rt.shutdown(None).await;
}

/// Test that very short lock timeout (1 second) works correctly
#[tokio::test]
async fn very_short_lock_timeout_works() {
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;

    let orch = |ctx: OrchestrationContext, _input: String| async move {
        let result = ctx.schedule_activity("FastActivity", "data").into_activity().await?;
        Ok(result)
    };

    let acts = ActivityRegistry::builder()
        .register(
            "FastActivity",
            |_ctx: duroxide::ActivityContext, input: String| async move {
                // Fast activity - completes well within 1 second
                Ok(format!("processed: {}", input))
            },
        )
        .build();

    let reg = OrchestrationRegistry::builder().register("TestOrch", orch).build();

    // Use very short lock timeouts (1 second)
    let options = RuntimeOptions {
        orchestrator_lock_timeout_secs: 1,
        worker_lock_timeout_secs: 1,
        ..Default::default()
    };

    let rt = runtime::Runtime::start_with_options(store.clone(), Arc::new(acts), reg, options).await;
    let client = Client::new(store.clone());

    let inst = "inst-short-timeout";
    client.start_orchestration(inst, "TestOrch", "").await.unwrap();

    // Should complete successfully even with short timeout (activity is fast)
    let status = client
        .wait_for_orchestration(inst, Duration::from_secs(5))
        .await
        .unwrap();

    match status {
        runtime::OrchestrationStatus::Completed { output } => {
            assert_eq!(output, "processed: data");
        }
        runtime::OrchestrationStatus::Failed { details } => {
            panic!("Orchestration failed: {}", details.display_message());
        }
        _ => panic!("Unexpected orchestration status"),
    }

    rt.shutdown(None).await;
}
