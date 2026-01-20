//! End-to-end tests for simplified replay mode.
//!
//! These tests validate the new commands-vs-history replay model
//! against real orchestrations using the runtime with
//! `use_simplified_replay: true`.

use duroxide::runtime::registry::ActivityRegistry;
use duroxide::runtime::{self, RuntimeOptions};
use duroxide::{ActivityContext, Client, OrchestrationContext, OrchestrationRegistry};
use std::sync::Arc;

mod common;

/// Simple test: single activity with simplified replay mode
#[tokio::test]
async fn simplified_single_activity() {
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;

    // Register a simple activity
    let activity_registry = ActivityRegistry::builder()
        .register("Echo", |_ctx: ActivityContext, input: String| async move {
            Ok(format!("echo: {input}"))
        })
        .build();

    // Simple orchestrator: call one activity
    let orchestration = |ctx: OrchestrationContext, input: String| async move {
        let result = ctx.simplified_schedule_activity("Echo", input).await?;
        Ok(result)
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("SimpleEcho", orchestration)
        .build();

    // Enable simplified replay mode
    let options = RuntimeOptions {
        use_simplified_replay: true,
        orchestration_concurrency: 1,
        worker_concurrency: 1,
        ..Default::default()
    };

    let rt = runtime::Runtime::start_with_options(
        store.clone(),
        Arc::new(activity_registry),
        orchestration_registry,
        options,
    )
    .await;

    let client = Client::new(store.clone());
    client
        .start_orchestration("simplified-single-1", "SimpleEcho", "hello")
        .await
        .unwrap();

    match client
        .wait_for_orchestration("simplified-single-1", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Completed { output } => {
            assert_eq!(output, "echo: hello");
        }
        runtime::OrchestrationStatus::Failed { details } => {
            panic!("orchestration failed: {}", details.display_message())
        }
        other => panic!("unexpected orchestration status: {:?}", other),
    }

    rt.shutdown(None).await;
}

/// Sequential activities with simplified replay mode
#[tokio::test]
async fn simplified_sequential_activities() {
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;

    // Register activities
    let activity_registry = ActivityRegistry::builder()
        .register("Add", |_ctx: ActivityContext, input: String| async move {
            let n: i32 = input.parse().unwrap();
            Ok((n + 1).to_string())
        })
        .register("Double", |_ctx: ActivityContext, input: String| async move {
            let n: i32 = input.parse().unwrap();
            Ok((n * 2).to_string())
        })
        .build();

    // Orchestrator: call two activities in sequence
    let orchestration = |ctx: OrchestrationContext, input: String| async move {
        // input = "5" -> add 1 -> "6" -> double -> "12"
        let added = ctx.simplified_schedule_activity("Add", input).await?;
        let doubled = ctx.simplified_schedule_activity("Double", added).await?;
        Ok(doubled)
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("Sequential", orchestration)
        .build();

    // Enable simplified replay mode
    let options = RuntimeOptions {
        use_simplified_replay: true,
        orchestration_concurrency: 1,
        worker_concurrency: 1,
        ..Default::default()
    };

    let rt = runtime::Runtime::start_with_options(
        store.clone(),
        Arc::new(activity_registry),
        orchestration_registry,
        options,
    )
    .await;

    let client = Client::new(store.clone());
    client
        .start_orchestration("simplified-seq-1", "Sequential", "5")
        .await
        .unwrap();

    match client
        .wait_for_orchestration("simplified-seq-1", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Completed { output } => {
            assert_eq!(output, "12"); // 5 + 1 = 6, 6 * 2 = 12
        }
        runtime::OrchestrationStatus::Failed { details } => {
            panic!("orchestration failed: {}", details.display_message())
        }
        other => panic!("unexpected orchestration status: {:?}", other),
    }

    rt.shutdown(None).await;
}

/// Timer with simplified replay mode  
#[tokio::test]
async fn simplified_timer() {
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;

    // No activities needed for timer test
    let activity_registry = ActivityRegistry::builder().build();

    // Orchestrator: wait for a short timer, then complete
    let orchestration = |ctx: OrchestrationContext, input: String| async move {
        ctx.simplified_schedule_timer(std::time::Duration::from_millis(10)).await;
        Ok(format!("done: {input}"))
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("TimerTest", orchestration)
        .build();

    // Enable simplified replay mode
    let options = RuntimeOptions {
        use_simplified_replay: true,
        orchestration_concurrency: 1,
        worker_concurrency: 1,
        ..Default::default()
    };

    let rt = runtime::Runtime::start_with_options(
        store.clone(),
        Arc::new(activity_registry),
        orchestration_registry,
        options,
    )
    .await;

    let client = Client::new(store.clone());
    client
        .start_orchestration("simplified-timer-1", "TimerTest", "test")
        .await
        .unwrap();

    match client
        .wait_for_orchestration("simplified-timer-1", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Completed { output } => {
            assert_eq!(output, "done: test");
        }
        runtime::OrchestrationStatus::Failed { details } => {
            panic!("orchestration failed: {}", details.display_message())
        }
        other => panic!("unexpected orchestration status: {:?}", other),
    }

    rt.shutdown(None).await;
}

/// Test that trace_simplified only emits traces once (not during replay)
#[tokio::test]
async fn simplified_trace_emits_once() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tracing_subscriber::layer::SubscriberExt;
    
    // Counter to track how many traces we receive
    static TRACE_COUNT: AtomicUsize = AtomicUsize::new(0);
    TRACE_COUNT.store(0, Ordering::SeqCst);
    
    // Custom layer to count trace events
    struct CountingLayer;
    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for CountingLayer {
        fn on_event(&self, event: &tracing::Event<'_>, _ctx: tracing_subscriber::layer::Context<'_, S>) {
            // Only count events from duroxide::orchestration with our specific message
            if event.metadata().target() == "duroxide::orchestration" {
                // Check if this is our trace message
                let mut found = false;
                event.record(&mut |field: &tracing::field::Field, value: &dyn std::fmt::Debug| {
                    if field.name() == "message" && format!("{:?}", value).contains("TRACE_MARKER_123") {
                        found = true;
                    }
                });
                if found {
                    TRACE_COUNT.fetch_add(1, Ordering::SeqCst);
                }
            }
        }
    }
    
    let subscriber = tracing_subscriber::registry().with(CountingLayer);
    let _guard = tracing::subscriber::set_default(subscriber);
    
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;

    let activity_registry = ActivityRegistry::builder()
        .register("Slow", |_ctx: ActivityContext, input: String| async move {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            Ok(format!("slow: {input}"))
        })
        .build();

    // Orchestrator: traces before and after activity
    let orchestration = |ctx: OrchestrationContext, input: String| async move {
        // This trace should only emit ONCE (not during replay)
        ctx.trace_info_simplified("TRACE_MARKER_123 before activity");
        
        let result = ctx.simplified_schedule_activity("Slow", input).await?;
        
        // This trace should also only emit ONCE
        ctx.trace_info_simplified("TRACE_MARKER_123 after activity");
        
        Ok(result)
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("TraceTest", orchestration)
        .build();

    let options = RuntimeOptions {
        use_simplified_replay: true,
        orchestration_concurrency: 1,
        worker_concurrency: 1,
        ..Default::default()
    };

    let rt = runtime::Runtime::start_with_options(
        store.clone(),
        Arc::new(activity_registry),
        orchestration_registry,
        options,
    )
    .await;

    let client = Client::new(store.clone());
    client
        .start_orchestration("trace-test-1", "TraceTest", "hello")
        .await
        .unwrap();

    match client
        .wait_for_orchestration("trace-test-1", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Completed { output } => {
            assert_eq!(output, "slow: hello");
        }
        runtime::OrchestrationStatus::Failed { details } => {
            panic!("orchestration failed: {}", details.display_message())
        }
        other => panic!("unexpected orchestration status: {:?}", other),
    }

    rt.shutdown(None).await;
    
    // We should have exactly 2 traces: one before activity, one after
    // If replay caused duplicate traces, we'd have more
    let count = TRACE_COUNT.load(Ordering::SeqCst);
    assert_eq!(count, 2, "Expected 2 traces (before + after activity), got {count}. Traces may have been duplicated during replay.");
}

/// Test is_replaying() returns correct values at different execution points
#[tokio::test]
async fn simplified_is_replaying_transitions() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    
    // Counters to track replaying state at different points
    static REPLAYING_AT_START: AtomicUsize = AtomicUsize::new(0);
    static NOT_REPLAYING_AT_START: AtomicUsize = AtomicUsize::new(0);
    static REPLAYING_AFTER_ACTIVITY: AtomicUsize = AtomicUsize::new(0);
    static NOT_REPLAYING_AFTER_ACTIVITY: AtomicUsize = AtomicUsize::new(0);
    
    REPLAYING_AT_START.store(0, Ordering::SeqCst);
    NOT_REPLAYING_AT_START.store(0, Ordering::SeqCst);
    REPLAYING_AFTER_ACTIVITY.store(0, Ordering::SeqCst);
    NOT_REPLAYING_AFTER_ACTIVITY.store(0, Ordering::SeqCst);
    
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;

    let activity_registry = ActivityRegistry::builder()
        .register("Work", |_ctx: ActivityContext, input: String| async move {
            Ok(format!("done: {input}"))
        })
        .build();

    // Orchestrator: check is_replaying at different points
    let orchestration = |ctx: OrchestrationContext, input: String| async move {
        // Check replaying state at start
        if ctx.is_replaying() {
            REPLAYING_AT_START.fetch_add(1, Ordering::SeqCst);
        } else {
            NOT_REPLAYING_AT_START.fetch_add(1, Ordering::SeqCst);
        }
        
        let result = ctx.simplified_schedule_activity("Work", input).await?;
        
        // Check replaying state after activity completes
        if ctx.is_replaying() {
            REPLAYING_AFTER_ACTIVITY.fetch_add(1, Ordering::SeqCst);
        } else {
            NOT_REPLAYING_AFTER_ACTIVITY.fetch_add(1, Ordering::SeqCst);
        }
        
        Ok(result)
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("ReplayingTest", orchestration)
        .build();

    let options = RuntimeOptions {
        use_simplified_replay: true,
        orchestration_concurrency: 1,
        worker_concurrency: 1,
        ..Default::default()
    };

    let rt = runtime::Runtime::start_with_options(
        store.clone(),
        Arc::new(activity_registry),
        orchestration_registry,
        options,
    )
    .await;

    let client = Client::new(store.clone());
    client
        .start_orchestration("replaying-test-1", "ReplayingTest", "hello")
        .await
        .unwrap();

    match client
        .wait_for_orchestration("replaying-test-1", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Completed { output } => {
            assert_eq!(output, "done: hello");
        }
        runtime::OrchestrationStatus::Failed { details } => {
            panic!("orchestration failed: {}", details.display_message())
        }
        other => panic!("unexpected orchestration status: {:?}", other),
    }

    rt.shutdown(None).await;
    
    // Execution breakdown:
    // Turn 1: Start, is_replaying=false (no prior history), schedule activity, yield
    // Turn 2: Replay (is_replaying=true at start), activity complete, is_replaying=false after
    
    // At start: Turn 1 has is_replaying=false, Turn 2 has is_replaying=true
    assert_eq!(NOT_REPLAYING_AT_START.load(Ordering::SeqCst), 1, 
        "Expected 1 non-replaying check at start (turn 1)");
    assert_eq!(REPLAYING_AT_START.load(Ordering::SeqCst), 1, 
        "Expected 1 replaying check at start (turn 2)");
    
    // After activity: only executed in Turn 2 with is_replaying=false
    assert_eq!(NOT_REPLAYING_AFTER_ACTIVITY.load(Ordering::SeqCst), 1, 
        "Expected 1 non-replaying check after activity");
    assert_eq!(REPLAYING_AFTER_ACTIVITY.load(Ordering::SeqCst), 0, 
        "Expected 0 replaying checks after activity");
}

// =============================================================================
// Complex E2E Tests
// =============================================================================

/// Fan-out/fan-in: parallel activities with ctx.simplified_schedule_activity() and ctx.simplified_join()
#[tokio::test]
async fn simplified_fan_out_fan_in() {
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;

    let activity_registry = ActivityRegistry::builder()
        .register("Process", |_ctx: ActivityContext, input: String| async move {
            // Simulate work
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            Ok(format!("processed:{input}"))
        })
        .build();

    // Orchestrator: fan out to 3 activities, fan in results
    let orchestration = |ctx: OrchestrationContext, _input: String| async move {
        // Use simplified_schedule_activity which returns a regular future directly
        let f1 = ctx.simplified_schedule_activity("Process", "A");
        let f2 = ctx.simplified_schedule_activity("Process", "B");
        let f3 = ctx.simplified_schedule_activity("Process", "C");
        
        // Use simplified_join which works with normal futures
        let results = ctx.simplified_join(vec![f1, f2, f3]).await;
        
        // Collect results, handling errors
        let mut outputs = Vec::new();
        for r in results {
            outputs.push(r?);
        }
        outputs.sort();
        Ok(outputs.join(","))
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("FanOut", orchestration)
        .build();

    let options = RuntimeOptions {
        use_simplified_replay: true,
        orchestration_concurrency: 1,
        worker_concurrency: 3, // Allow parallel activity execution
        ..Default::default()
    };

    let rt = runtime::Runtime::start_with_options(
        store.clone(),
        Arc::new(activity_registry),
        orchestration_registry,
        options,
    )
    .await;

    let client = Client::new(store.clone());
    client
        .start_orchestration("fanout-1", "FanOut", "input")
        .await
        .unwrap();

    match client
        .wait_for_orchestration("fanout-1", std::time::Duration::from_secs(10))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Completed { output } => {
            assert_eq!(output, "processed:A,processed:B,processed:C");
        }
        runtime::OrchestrationStatus::Failed { details } => {
            panic!("orchestration failed: {}", details.display_message())
        }
        other => panic!("unexpected orchestration status: {:?}", other),
    }

    rt.shutdown(None).await;
}

/// Sub-orchestration test
#[tokio::test]
async fn simplified_sub_orchestration() {
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;

    let activity_registry = ActivityRegistry::builder()
        .register("Double", |_ctx: ActivityContext, input: String| async move {
            let n: i32 = input.parse().unwrap();
            Ok((n * 2).to_string())
        })
        .build();

    // Child orchestration: doubles the input
    let child = |ctx: OrchestrationContext, input: String| async move {
        let result = ctx.simplified_schedule_activity("Double", input).await?;
        Ok(result)
    };

    // Parent orchestration: calls child twice
    let parent = |ctx: OrchestrationContext, input: String| async move {
        let first = ctx.simplified_schedule_sub_orchestration("Child", input).await?;
        let second = ctx.simplified_schedule_sub_orchestration("Child", first).await?;
        Ok(second)
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("Child", child)
        .register("Parent", parent)
        .build();

    let options = RuntimeOptions {
        use_simplified_replay: true,
        orchestration_concurrency: 2, // Allow parent and child to run
        worker_concurrency: 1,
        ..Default::default()
    };

    let rt = runtime::Runtime::start_with_options(
        store.clone(),
        Arc::new(activity_registry),
        orchestration_registry,
        options,
    )
    .await;

    let client = Client::new(store.clone());
    client
        .start_orchestration("parent-1", "Parent", "5")
        .await
        .unwrap();

    match client
        .wait_for_orchestration("parent-1", std::time::Duration::from_secs(10))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Completed { output } => {
            // 5 * 2 = 10, 10 * 2 = 20
            assert_eq!(output, "20");
        }
        runtime::OrchestrationStatus::Failed { details } => {
            panic!("orchestration failed: {}", details.display_message())
        }
        other => panic!("unexpected orchestration status: {:?}", other),
    }

    rt.shutdown(None).await;
}

/// External event test
#[tokio::test]
async fn simplified_external_event() {
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;

    let activity_registry = ActivityRegistry::builder().build();

    // Orchestration waits for external event
    let orchestration = |ctx: OrchestrationContext, _input: String| async move {
        let data = ctx.simplified_schedule_wait("approval").await;
        Ok(format!("approved:{data}"))
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("WaitForApproval", orchestration)
        .build();

    let options = RuntimeOptions {
        use_simplified_replay: true,
        orchestration_concurrency: 1,
        worker_concurrency: 1,
        ..Default::default()
    };

    let rt = runtime::Runtime::start_with_options(
        store.clone(),
        Arc::new(activity_registry),
        orchestration_registry,
        options,
    )
    .await;

    let client = Client::new(store.clone());
    client
        .start_orchestration("wait-1", "WaitForApproval", "")
        .await
        .unwrap();

    // Wait for the subscription to be persisted in history before raising event
    assert!(
        common::wait_for_subscription(store.clone(), "wait-1", "approval", 2000).await,
        "subscription should be recorded in history"
    );

    // Raise the external event
    client
        .raise_event("wait-1", "approval", "yes")
        .await
        .unwrap();

    match client
        .wait_for_orchestration("wait-1", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Completed { output } => {
            assert_eq!(output, "approved:yes");
        }
        runtime::OrchestrationStatus::Failed { details } => {
            panic!("orchestration failed: {}", details.display_message())
        }
        other => panic!("unexpected orchestration status: {:?}", other),
    }

    rt.shutdown(None).await;
}

/// Continue-as-new test
#[tokio::test]
async fn simplified_continue_as_new() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    
    static EXECUTION_COUNT: AtomicUsize = AtomicUsize::new(0);
    EXECUTION_COUNT.store(0, Ordering::SeqCst);
    
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;

    let activity_registry = ActivityRegistry::builder()
        .register("Increment", |_ctx: ActivityContext, input: String| async move {
            let n: i32 = input.parse().unwrap();
            Ok((n + 1).to_string())
        })
        .build();

    // Orchestration that continues-as-new until reaching target
    let orchestration = |ctx: OrchestrationContext, input: String| async move {
        EXECUTION_COUNT.fetch_add(1, Ordering::SeqCst);
        
        let current: i32 = input.parse().unwrap();
        if current >= 3 {
            return Ok(format!("done:{current}"));
        }
        
        let next = ctx.simplified_schedule_activity("Increment", input).await?;
        return ctx.continue_as_new(next).await;  // Terminates execution
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("Counter", orchestration)
        .build();

    let options = RuntimeOptions {
        use_simplified_replay: true,
        orchestration_concurrency: 1,
        worker_concurrency: 1,
        ..Default::default()
    };

    let rt = runtime::Runtime::start_with_options(
        store.clone(),
        Arc::new(activity_registry),
        orchestration_registry,
        options,
    )
    .await;

    let client = Client::new(store.clone());
    client
        .start_orchestration("can-1", "Counter", "0")
        .await
        .unwrap();

    match client
        .wait_for_orchestration("can-1", std::time::Duration::from_secs(10))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Completed { output } => {
            // 0 -> 1 -> 2 -> 3 (done)
            assert_eq!(output, "done:3");
            // Each continue-as-new execution runs the orchestration twice (schedule turn + complete turn)
            // Except the final execution which completes immediately.
            // Executions: 0 (2 turns), 1 (2 turns), 2 (2 turns), 3 (1 turn) = 7 total invocations
            let count = EXECUTION_COUNT.load(Ordering::SeqCst);
            assert!(count >= 4, "Expected at least 4 orchestration invocations, got {count}");
        }
        runtime::OrchestrationStatus::Failed { details } => {
            panic!("orchestration failed: {}", details.display_message())
        }
        other => panic!("unexpected orchestration status: {:?}", other),
    }

    rt.shutdown(None).await;
}

/// Activity failure test
#[tokio::test]
async fn simplified_activity_failure() {
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;

    let activity_registry = ActivityRegistry::builder()
        .register("FailingActivity", |_ctx: ActivityContext, _input: String| async move {
            Err("intentional failure".to_string())
        })
        .register("SucceedingActivity", |_ctx: ActivityContext, input: String| async move {
            Ok(format!("success:{input}"))
        })
        .build();

    // Orchestration that handles activity failure
    let orchestration = |ctx: OrchestrationContext, input: String| async move {
        let result = ctx.simplified_schedule_activity("FailingActivity", input.clone()).await;
        match result {
            Ok(_) => Ok("unexpected success".to_string()),
            Err(e) => {
                // Activity failed, try fallback
                let fallback = ctx.simplified_schedule_activity("SucceedingActivity", input).await?;
                Ok(format!("fallback:{fallback},error:{e}"))
            }
        }
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("HandleFailure", orchestration)
        .build();

    let options = RuntimeOptions {
        use_simplified_replay: true,
        orchestration_concurrency: 1,
        worker_concurrency: 1,
        ..Default::default()
    };

    let rt = runtime::Runtime::start_with_options(
        store.clone(),
        Arc::new(activity_registry),
        orchestration_registry,
        options,
    )
    .await;

    let client = Client::new(store.clone());
    client
        .start_orchestration("fail-1", "HandleFailure", "test")
        .await
        .unwrap();

    match client
        .wait_for_orchestration("fail-1", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Completed { output } => {
            assert!(output.starts_with("fallback:success:test,error:"));
            assert!(output.contains("intentional failure"));
        }
        runtime::OrchestrationStatus::Failed { details } => {
            panic!("orchestration failed: {}", details.display_message())
        }
        other => panic!("unexpected orchestration status: {:?}", other),
    }

    rt.shutdown(None).await;
}

/// Select2 test: first activity wins, second is cancelled
#[tokio::test]
async fn simplified_select2_first_wins() {
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;

    let activity_registry = ActivityRegistry::builder()
        .register("Fast", |_ctx: ActivityContext, input: String| async move {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            Ok(format!("fast:{input}"))
        })
        .register("Slow", |_ctx: ActivityContext, input: String| async move {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            Ok(format!("slow:{input}"))
        })
        .build();

    // Orchestration: race two activities
    let orchestration = |ctx: OrchestrationContext, input: String| async move {
        let fast = ctx.simplified_schedule_activity("Fast", input.clone());
        let slow = ctx.simplified_schedule_activity("Slow", input);
        
        // Use simplified_select2 which works with normal futures
        let (winner_index, result) = ctx.simplified_select2(fast, slow).await;
        let output = result?;
        Ok(format!("winner:{winner_index},output:{output}"))
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("Race", orchestration)
        .build();

    let options = RuntimeOptions {
        use_simplified_replay: true,
        orchestration_concurrency: 1,
        worker_concurrency: 2,
        ..Default::default()
    };

    let rt = runtime::Runtime::start_with_options(
        store.clone(),
        Arc::new(activity_registry),
        orchestration_registry,
        options,
    )
    .await;

    let client = Client::new(store.clone());
    client
        .start_orchestration("race-1", "Race", "test")
        .await
        .unwrap();

    match client
        .wait_for_orchestration("race-1", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Completed { output } => {
            // Fast should win (index 0)
            assert_eq!(output, "winner:0,output:fast:test");
        }
        runtime::OrchestrationStatus::Failed { details } => {
            panic!("orchestration failed: {}", details.display_message())
        }
        other => panic!("unexpected orchestration status: {:?}", other),
    }

    rt.shutdown(None).await;
}

/// Timer with activity test
#[tokio::test]
async fn simplified_timer_with_activity() {
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;

    let activity_registry = ActivityRegistry::builder()
        .register("Work", |_ctx: ActivityContext, input: String| async move {
            Ok(format!("worked:{input}"))
        })
        .build();

    // Orchestration: timer, then activity, then timer, then done
    let orchestration = |ctx: OrchestrationContext, input: String| async move {
        ctx.simplified_schedule_timer(std::time::Duration::from_millis(10)).await;
        
        let result = ctx.simplified_schedule_activity("Work", input).await?;
        
        ctx.simplified_schedule_timer(std::time::Duration::from_millis(10)).await;
        
        Ok(format!("done:{result}"))
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("TimerWork", orchestration)
        .build();

    let options = RuntimeOptions {
        use_simplified_replay: true,
        orchestration_concurrency: 1,
        worker_concurrency: 1,
        ..Default::default()
    };

    let rt = runtime::Runtime::start_with_options(
        store.clone(),
        Arc::new(activity_registry),
        orchestration_registry,
        options,
    )
    .await;

    let client = Client::new(store.clone());
    client
        .start_orchestration("tw-1", "TimerWork", "test")
        .await
        .unwrap();

    match client
        .wait_for_orchestration("tw-1", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Completed { output } => {
            assert_eq!(output, "done:worked:test");
        }
        runtime::OrchestrationStatus::Failed { details } => {
            panic!("orchestration failed: {}", details.display_message())
        }
        other => panic!("unexpected orchestration status: {:?}", other),
    }

    rt.shutdown(None).await;
}

/// System calls (new_guid, utcnow) test
#[tokio::test]
async fn simplified_system_calls() {
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;

    let activity_registry = ActivityRegistry::builder().build();

    // Orchestration that uses system calls
    let orchestration = |ctx: OrchestrationContext, _input: String| async move {
        let guid = ctx.new_guid().await?;
        let now = ctx.utcnow().await?;
        
        // Validate GUID format (UUID v4 format: 8-4-4-4-12)
        assert_eq!(guid.len(), 36);
        assert!(guid.chars().filter(|c| *c == '-').count() == 4);
        
        // Validate timestamp is reasonable (after 2020)
        let epoch = std::time::SystemTime::UNIX_EPOCH;
        let duration = now.duration_since(epoch).unwrap();
        let millis = duration.as_millis() as u64;
        assert!(millis > 1577836800000); // 2020-01-01
        
        Ok(format!("guid_len:{},now_valid:true", guid.len()))
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("SysCalls", orchestration)
        .build();

    let options = RuntimeOptions {
        use_simplified_replay: true,
        orchestration_concurrency: 1,
        worker_concurrency: 1,
        ..Default::default()
    };

    let rt = runtime::Runtime::start_with_options(
        store.clone(),
        Arc::new(activity_registry),
        orchestration_registry,
        options,
    )
    .await;

    let client = Client::new(store.clone());
    client
        .start_orchestration("syscall-1", "SysCalls", "")
        .await
        .unwrap();

    match client
        .wait_for_orchestration("syscall-1", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Completed { output } => {
            assert_eq!(output, "guid_len:36,now_valid:true");
        }
        runtime::OrchestrationStatus::Failed { details } => {
            panic!("orchestration failed: {}", details.display_message())
        }
        other => panic!("unexpected orchestration status: {:?}", other),
    }

    rt.shutdown(None).await;
}

// =============================================================================
// Ported from e2e_samples.rs - Control Flow & Loops
// =============================================================================

/// Basic control flow: branch on a flag returned by an activity.
#[tokio::test]
async fn simplified_basic_control_flow() {
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;

    let activity_registry = ActivityRegistry::builder()
        .register("GetFlag", |_ctx: ActivityContext, _input: String| async move {
            Ok("yes".to_string())
        })
        .register("SayYes", |_ctx: ActivityContext, _in: String| async move {
            Ok("picked_yes".to_string())
        })
        .register("SayNo", |_ctx: ActivityContext, _in: String| async move {
            Ok("picked_no".to_string())
        })
        .build();

    let orchestration = |ctx: OrchestrationContext, _input: String| async move {
        let flag = ctx.simplified_schedule_activity("GetFlag", "").await?;
        ctx.trace_info_simplified(format!("control_flow flag decided = {flag}"));
        if flag == "yes" {
            Ok(ctx.simplified_schedule_activity("SayYes", "").await?)
        } else {
            Ok(ctx.simplified_schedule_activity("SayNo", "").await?)
        }
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("ControlFlow", orchestration)
        .build();

    let options = RuntimeOptions {
        use_simplified_replay: true,
        orchestration_concurrency: 1,
        worker_concurrency: 1,
        ..Default::default()
    };

    let rt = runtime::Runtime::start_with_options(
        store.clone(),
        Arc::new(activity_registry),
        orchestration_registry,
        options,
    )
    .await;

    let client = Client::new(store.clone());
    client
        .start_orchestration("control-flow-1", "ControlFlow", "")
        .await
        .unwrap();

    match client
        .wait_for_orchestration("control-flow-1", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Completed { output } => assert_eq!(output, "picked_yes"),
        runtime::OrchestrationStatus::Failed { details } => {
            panic!("orchestration failed: {}", details.display_message())
        }
        other => panic!("unexpected orchestration status: {:?}", other),
    }

    rt.shutdown(None).await;
}

/// Loops and accumulation: call an activity repeatedly and build up a value.
#[tokio::test]
async fn simplified_loop_accumulation() {
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;

    let activity_registry = ActivityRegistry::builder()
        .register("Append", |_ctx: ActivityContext, input: String| async move {
            Ok(format!("{input}x"))
        })
        .build();

    let orchestration = |ctx: OrchestrationContext, _input: String| async move {
        let mut acc = String::from("start");
        for i in 0..3 {
            acc = ctx.simplified_schedule_activity("Append", acc).await?;
            ctx.trace_info_simplified(format!("loop iteration {i} completed acc={acc}"));
        }
        Ok(acc)
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("LoopOrchestration", orchestration)
        .build();

    let options = RuntimeOptions {
        use_simplified_replay: true,
        orchestration_concurrency: 1,
        worker_concurrency: 1,
        ..Default::default()
    };

    let rt = runtime::Runtime::start_with_options(
        store.clone(),
        Arc::new(activity_registry),
        orchestration_registry,
        options,
    )
    .await;

    let client = Client::new(store.clone());
    client
        .start_orchestration("loop-1", "LoopOrchestration", "")
        .await
        .unwrap();

    match client
        .wait_for_orchestration("loop-1", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Completed { output } => assert_eq!(output, "startxxx"),
        runtime::OrchestrationStatus::Failed { details } => {
            panic!("orchestration failed: {}", details.display_message())
        }
        other => panic!("unexpected orchestration status: {:?}", other),
    }

    rt.shutdown(None).await;
}

// =============================================================================
// Ported from e2e_samples.rs - Error Handling
// =============================================================================

/// Error handling and compensation: recover from a failed activity.
#[tokio::test]
async fn simplified_error_handling_compensation() {
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;

    let activity_registry = ActivityRegistry::builder()
        .register("Fragile", |_ctx: ActivityContext, input: String| async move {
            if input == "bad" {
                Err("boom".to_string())
            } else {
                Ok("ok".to_string())
            }
        })
        .register("Recover", |_ctx: ActivityContext, _input: String| async move {
            Ok("recovered".to_string())
        })
        .build();

    let orchestration = |ctx: OrchestrationContext, _input: String| async move {
        match ctx.simplified_schedule_activity("Fragile", "bad").await {
            Ok(v) => {
                ctx.trace_info_simplified(format!("fragile succeeded value={v}"));
                Ok(v)
            }
            Err(e) => {
                ctx.trace_warn_simplified(format!("fragile failed error={e}"));
                let rec = ctx.simplified_schedule_activity("Recover", "").await?;
                if rec != "recovered" {
                    ctx.trace_error_simplified(format!("unexpected recovery value={rec}"));
                }
                Ok(rec)
            }
        }
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("ErrorHandling", orchestration)
        .build();

    let options = RuntimeOptions {
        use_simplified_replay: true,
        orchestration_concurrency: 1,
        worker_concurrency: 1,
        ..Default::default()
    };

    let rt = runtime::Runtime::start_with_options(
        store.clone(),
        Arc::new(activity_registry),
        orchestration_registry,
        options,
    )
    .await;

    let client = Client::new(store.clone());
    client
        .start_orchestration("error-handling-1", "ErrorHandling", "")
        .await
        .unwrap();

    match client
        .wait_for_orchestration("error-handling-1", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Completed { output } => assert_eq!(output, "recovered"),
        runtime::OrchestrationStatus::Failed { details } => {
            panic!("orchestration failed: {}", details.display_message())
        }
        other => panic!("unexpected orchestration status: {:?}", other),
    }

    rt.shutdown(None).await;
}

/// Basic error handling: activity failure propagation
#[tokio::test]
async fn simplified_basic_error_handling() {
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;

    let activity_registry = ActivityRegistry::builder()
        .register("ValidateInput", |_ctx: ActivityContext, input: String| async move {
            if input.is_empty() {
                Err("Input cannot be empty".to_string())
            } else {
                Ok(format!("Valid: {input}"))
            }
        })
        .build();

    let orchestration = |ctx: OrchestrationContext, input: String| async move {
        ctx.trace_info_simplified("Starting validation");
        let result = ctx.simplified_schedule_activity("ValidateInput", input).await?;
        ctx.trace_info_simplified(format!("Validation result: {result}"));
        Ok(result)
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("BasicErrorHandling", orchestration)
        .build();

    let options = RuntimeOptions {
        use_simplified_replay: true,
        orchestration_concurrency: 1,
        worker_concurrency: 1,
        ..Default::default()
    };

    let rt = runtime::Runtime::start_with_options(
        store.clone(),
        Arc::new(activity_registry),
        orchestration_registry,
        options,
    )
    .await;

    let client = Client::new(store.clone());

    // Test successful case
    client
        .start_orchestration("basic-error-1", "BasicErrorHandling", "test")
        .await
        .unwrap();

    match client
        .wait_for_orchestration("basic-error-1", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Completed { output } => {
            assert_eq!(output, "Valid: test");
        }
        runtime::OrchestrationStatus::Failed { details } => {
            panic!("orchestration failed: {}", details.display_message())
        }
        other => panic!("unexpected orchestration status: {:?}", other),
    }

    // Test error case
    client
        .start_orchestration("basic-error-2", "BasicErrorHandling", "")
        .await
        .unwrap();

    match client
        .wait_for_orchestration("basic-error-2", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Failed { details } => {
            assert!(details.display_message().contains("Input cannot be empty"));
        }
        runtime::OrchestrationStatus::Completed { output } => {
            panic!("Expected failure but got success: {output}")
        }
        other => panic!("unexpected orchestration status: {:?}", other),
    }

    rt.shutdown(None).await;
}

// =============================================================================
// Ported from e2e_samples.rs - Timer & Race Patterns
// =============================================================================

/// Timeouts via racing a long-running activity against a timer.
#[tokio::test]
async fn simplified_timeout_with_timer_race() {
    use std::time::Duration;

    let (store, _temp_dir) = common::create_sqlite_store_disk().await;

    let activity_registry = ActivityRegistry::builder()
        .register("LongOp", |ctx: ActivityContext, _input: String| async move {
            ctx.trace_info("LongOp started");
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            ctx.trace_info("LongOp finished");
            Ok("done".to_string())
        })
        .build();

    let orchestration = |ctx: OrchestrationContext, _input: String| async move {
        // Race activity vs short timeout - both arms return Result<String,String>
        let activity = ctx.simplified_schedule_activity("LongOp", "");
        let timer = async {
            ctx.simplified_schedule_timer(Duration::from_millis(100)).await;
            Err::<String, String>("timeout".to_string())
        };

        let (idx, result) = ctx.simplified_select2(activity, timer).await;
        match (idx, result) {
            (0, Ok(s)) => Ok(s),
            (0, Err(e)) => Err(e),
            (1, Err(e)) => Err(e),
            (1, Ok(s)) => Ok(s),
            _ => unreachable!(),
        }
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("TimeoutSample", orchestration)
        .build();

    let options = RuntimeOptions {
        use_simplified_replay: true,
        orchestration_concurrency: 1,
        worker_concurrency: 1,
        ..Default::default()
    };

    let rt = runtime::Runtime::start_with_options(
        store.clone(),
        Arc::new(activity_registry),
        orchestration_registry,
        options,
    )
    .await;

    let client = Client::new(store.clone());
    client
        .start_orchestration("timeout-race-1", "TimeoutSample", "")
        .await
        .unwrap();

    match client
        .wait_for_orchestration("timeout-race-1", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Failed { details } => {
            assert_eq!(details.display_message(), "timeout")
        }
        runtime::OrchestrationStatus::Completed { output } => {
            panic!("expected timeout failure, got: {output}")
        }
        other => panic!("unexpected orchestration status: {:?}", other),
    }

    rt.shutdown(None).await;
}

/// Mixed race with select2: activity vs external event.
#[tokio::test]
async fn simplified_select2_activity_vs_external() {
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;

    let activity_registry = ActivityRegistry::builder()
        .register("Sleep", |ctx: ActivityContext, _input: String| async move {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            ctx.trace_info("Sleep activity finished");
            Ok("slept".to_string())
        })
        .build();

    let orchestration = |ctx: OrchestrationContext, _input: String| async move {
        // Wrap activity to return String (not Result) so types match
        let act = async {
            ctx.simplified_schedule_activity("Sleep", "")
                .await
                .unwrap_or_else(|e| format!("error:{e}"))
        };
        let evt = ctx.simplified_schedule_wait("Go");

        let (idx, result) = ctx.simplified_select2(act, evt).await;
        match idx {
            0 => Ok(format!("activity:{result}")),
            1 => Ok(format!("event:{result}")),
            _ => unreachable!(),
        }
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("Select2ActVsEvt", orchestration)
        .build();

    let options = RuntimeOptions {
        use_simplified_replay: true,
        orchestration_concurrency: 1,
        worker_concurrency: 1,
        ..Default::default()
    };

    let rt = runtime::Runtime::start_with_options(
        store.clone(),
        Arc::new(activity_registry),
        orchestration_registry,
        options,
    )
    .await;

    // Start orchestration, then raise external after subscription is recorded
    let store_for_wait = store.clone();
    tokio::spawn(async move {
        let sfw = store_for_wait.clone();
        let _ = common::wait_for_subscription(sfw.clone(), "select2-act-evt-1", "Go", 1000).await;
        let client = Client::new(sfw);
        let _ = client.raise_event("select2-act-evt-1", "Go", "ok").await;
    });

    let client = Client::new(store.clone());
    client
        .start_orchestration("select2-act-evt-1", "Select2ActVsEvt", "")
        .await
        .unwrap();

    let s = match client
        .wait_for_orchestration("select2-act-evt-1", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Completed { output } => output,
        runtime::OrchestrationStatus::Failed { details } => {
            panic!("orchestration failed: {}", details.display_message())
        }
        other => panic!("unexpected orchestration status: {:?}", other),
    };

    // External event should win because activity sleeps 300ms
    assert_eq!(s, "event:ok");

    rt.shutdown(None).await;
}

// =============================================================================
// Ported from e2e_samples.rs - Sub-Orchestrations
// =============================================================================

/// Sub-orchestrations: fan-out to multiple children and join.
#[tokio::test]
async fn simplified_sub_orchestration_fanout() {
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;

    let activity_registry = ActivityRegistry::builder()
        .register("Add", |_ctx: ActivityContext, input: String| async move {
            let mut it = input.split(',');
            let a = it.next().unwrap_or("0").parse::<i64>().unwrap_or(0);
            let b = it.next().unwrap_or("0").parse::<i64>().unwrap_or(0);
            Ok((a + b).to_string())
        })
        .build();

    let child_sum = |ctx: OrchestrationContext, input: String| async move {
        let s = ctx.simplified_schedule_activity("Add", input).await?;
        Ok(s)
    };

    let parent = |ctx: OrchestrationContext, _input: String| async move {
        let a = ctx.simplified_schedule_sub_orchestration("ChildSum", "1,2");
        let b = ctx.simplified_schedule_sub_orchestration("ChildSum", "3,4");
        let (r1, r2) = ctx.simplified_join2(a, b).await;
        let n1: i64 = r1?.parse().unwrap();
        let n2: i64 = r2?.parse().unwrap();
        Ok(format!("total={}", n1 + n2))
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("ChildSum", child_sum)
        .register("ParentFan", parent)
        .build();

    let options = RuntimeOptions {
        use_simplified_replay: true,
        orchestration_concurrency: 3, // Parent + 2 children
        worker_concurrency: 2,
        ..Default::default()
    };

    let rt = runtime::Runtime::start_with_options(
        store.clone(),
        Arc::new(activity_registry),
        orchestration_registry,
        options,
    )
    .await;

    let client = Client::new(store.clone());
    client
        .start_orchestration("sub-fan-1", "ParentFan", "")
        .await
        .unwrap();

    match client
        .wait_for_orchestration("sub-fan-1", std::time::Duration::from_secs(10))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Completed { output } => assert_eq!(output, "total=10"),
        runtime::OrchestrationStatus::Failed { details } => {
            panic!("orchestration failed: {}", details.display_message())
        }
        other => panic!("unexpected orchestration status: {:?}", other),
    }

    rt.shutdown(None).await;
}

/// Sub-orchestrations: chained (root -> mid -> leaf).
#[tokio::test]
async fn simplified_sub_orchestration_chained() {
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;

    let activity_registry = ActivityRegistry::builder()
        .register("AppendX", |_ctx: ActivityContext, input: String| async move {
            Ok(format!("{input}x"))
        })
        .build();

    let leaf = |ctx: OrchestrationContext, input: String| async move {
        Ok(ctx.simplified_schedule_activity("AppendX", input).await?)
    };

    let mid = |ctx: OrchestrationContext, input: String| async move {
        let r = ctx.simplified_schedule_sub_orchestration("Leaf", input).await?;
        Ok(format!("{r}-mid"))
    };

    let root = |ctx: OrchestrationContext, input: String| async move {
        let r = ctx.simplified_schedule_sub_orchestration("Mid", input).await?;
        Ok(format!("root:{r}"))
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("Leaf", leaf)
        .register("Mid", mid)
        .register("Root", root)
        .build();

    let options = RuntimeOptions {
        use_simplified_replay: true,
        orchestration_concurrency: 3, // Root + Mid + Leaf
        worker_concurrency: 1,
        ..Default::default()
    };

    let rt = runtime::Runtime::start_with_options(
        store.clone(),
        Arc::new(activity_registry),
        orchestration_registry,
        options,
    )
    .await;

    let client = Client::new(store.clone());
    client
        .start_orchestration("sub-chain-1", "Root", "a")
        .await
        .unwrap();

    match client
        .wait_for_orchestration("sub-chain-1", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Completed { output } => assert_eq!(output, "root:ax-mid"),
        runtime::OrchestrationStatus::Failed { details } => {
            panic!("orchestration failed: {}", details.display_message())
        }
        other => panic!("unexpected orchestration status: {:?}", other),
    }

    rt.shutdown(None).await;
}

/// Detached orchestration scheduling: start independent orchestrations without awaiting.
/// Note: schedule_orchestration is fire-and-forget and works in both modes.
#[tokio::test]
async fn simplified_detached_orchestration_scheduling() {
    use duroxide::OrchestrationStatus;
    use std::time::Duration;

    let (store, _temp_dir) = common::create_sqlite_store_disk().await;

    let activity_registry = ActivityRegistry::builder()
        .register("Echo", |_ctx: ActivityContext, input: String| async move {
            Ok(input)
        })
        .build();

    let chained = |ctx: OrchestrationContext, input: String| async move {
        ctx.simplified_schedule_timer(Duration::from_millis(5)).await;
        Ok(ctx.simplified_schedule_activity("Echo", input).await?)
    };

    let coordinator = |ctx: OrchestrationContext, _input: String| async move {
        // Fire-and-forget orchestrations using simplified API
        ctx.simplified_schedule_orchestration("Chained", "W1", "A");
        ctx.simplified_schedule_orchestration("Chained", "W2", "B");
        Ok("scheduled".to_string())
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("Chained", chained)
        .register("Coordinator", coordinator)
        .build();

    let options = RuntimeOptions {
        use_simplified_replay: true,
        orchestration_concurrency: 3,
        worker_concurrency: 2,
        ..Default::default()
    };

    let rt = runtime::Runtime::start_with_options(
        store.clone(),
        Arc::new(activity_registry),
        orchestration_registry,
        options,
    )
    .await;

    let client = Client::new(store.clone());
    client
        .start_orchestration("CoordinatorRoot", "Coordinator", "")
        .await
        .unwrap();

    match client
        .wait_for_orchestration("CoordinatorRoot", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Completed { output } => assert_eq!(output, "scheduled"),
        runtime::OrchestrationStatus::Failed { details } => {
            panic!("orchestration failed: {}", details.display_message())
        }
        other => panic!("unexpected orchestration status: {:?}", other),
    }

    // The scheduled instances are plain W1/W2 (no prefixing)
    let insts = vec!["W1".to_string(), "W2".to_string()];
    for inst in insts {
        // Give more time for detached orchestrations to be picked up
        match client
            .wait_for_orchestration(&inst, std::time::Duration::from_secs(10))
            .await
            .unwrap()
        {
            OrchestrationStatus::Completed { output } => {
                assert!(output == "A" || output == "B", "Expected A or B but got {output}");
            }
            OrchestrationStatus::Failed { details } => {
                panic!("scheduled orchestration failed: {}", details.display_message())
            }
            other => panic!("unexpected orchestration status: {:?}", other),
        }
    }

    rt.shutdown(None).await;
}

// =============================================================================
// Ported from e2e_samples.rs - Typed APIs
// =============================================================================

/// Typed activity + typed orchestration: Add two numbers and return a struct
#[tokio::test]
async fn simplified_typed_activity_and_orchestration() {
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct AddReq {
        a: i32,
        b: i32,
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct AddRes {
        sum: i32,
    }

    let (store, _temp_dir) = common::create_sqlite_store_disk().await;

    let activity_registry = ActivityRegistry::builder()
        .register_typed::<AddReq, AddRes, _, _>("Add", |_ctx: ActivityContext, req| async move {
            Ok(AddRes { sum: req.a + req.b })
        })
        .build();

    let orchestration = |ctx: OrchestrationContext, req: AddReq| async move {
        let out: AddRes = ctx.simplified_schedule_activity_typed::<AddReq, AddRes>("Add", &req).await?;
        Ok(out)
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register_typed::<AddReq, AddRes, _, _>("Adder", orchestration)
        .build();

    let options = RuntimeOptions {
        use_simplified_replay: true,
        orchestration_concurrency: 1,
        worker_concurrency: 1,
        ..Default::default()
    };

    let rt = runtime::Runtime::start_with_options(
        store.clone(),
        Arc::new(activity_registry),
        orchestration_registry,
        options,
    )
    .await;

    let client = Client::new(store.clone());
    client
        .start_orchestration_typed::<AddReq>("typed-add-1", "Adder", AddReq { a: 2, b: 3 })
        .await
        .unwrap();

    match client
        .wait_for_orchestration_typed::<AddRes>("typed-add-1", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        Ok(result) => assert_eq!(result, AddRes { sum: 5 }),
        Err(error) => panic!("orchestration failed: {error}"),
    }

    rt.shutdown(None).await;
}

/// Typed external event sample: await typed payload from an event
#[tokio::test]
async fn simplified_typed_event() {
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct Ack {
        ok: bool,
    }

    let (store, _temp_dir) = common::create_sqlite_store_disk().await;

    let activity_registry = ActivityRegistry::builder().build();

    let orch = |ctx: OrchestrationContext, _in: ()| async move {
        let ack: Ack = ctx.simplified_schedule_wait_typed::<Ack>("Ready").await;
        Ok::<_, String>(serde_json::to_string(&ack).unwrap())
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register_typed::<(), String, _, _>("WaitAck", orch)
        .build();

    let options = RuntimeOptions {
        use_simplified_replay: true,
        orchestration_concurrency: 1,
        worker_concurrency: 1,
        ..Default::default()
    };

    let rt = runtime::Runtime::start_with_options(
        store.clone(),
        Arc::new(activity_registry),
        orchestration_registry,
        options,
    )
    .await;

    let store_for_wait = store.clone();
    tokio::spawn(async move {
        let sfw = store_for_wait.clone();
        let _ = common::wait_for_subscription(sfw.clone(), "typed-ack-1", "Ready", 1000).await;
        let payload = serde_json::to_string(&Ack { ok: true }).unwrap();
        let client = Client::new(sfw);
        let _ = client.raise_event("typed-ack-1", "Ready", payload).await;
    });

    let client = Client::new(store.clone());
    client
        .start_orchestration_typed::<()>("typed-ack-1", "WaitAck", ())
        .await
        .unwrap();

    match client
        .wait_for_orchestration_typed::<String>("typed-ack-1", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        Ok(result) => {
            let expected = serde_json::to_string(&Ack { ok: true }).unwrap();
            assert_eq!(result, expected);
        }
        Err(error) => panic!("orchestration failed: {error}"),
    }

    rt.shutdown(None).await;
}

// =============================================================================
// Ported from e2e_samples.rs - Status Polling
// =============================================================================

/// Status polling: start an orchestration and poll its status until completion.
#[tokio::test]
async fn simplified_status_polling() {
    use duroxide::OrchestrationStatus;

    let (store, _temp_dir) = common::create_sqlite_store_disk().await;

    let activity_registry = ActivityRegistry::builder().build();
    let orchestration = |ctx: OrchestrationContext, _input: String| async move {
        ctx.simplified_schedule_timer(std::time::Duration::from_millis(20)).await;
        Ok("done".to_string())
    };
    let orchestration_registry = OrchestrationRegistry::builder()
        .register("StatusSample", orchestration)
        .build();

    let options = RuntimeOptions {
        use_simplified_replay: true,
        orchestration_concurrency: 1,
        worker_concurrency: 1,
        ..Default::default()
    };

    let rt = runtime::Runtime::start_with_options(
        store.clone(),
        Arc::new(activity_registry),
        orchestration_registry,
        options,
    )
    .await;

    let client = Client::new(store.clone());
    client
        .start_orchestration("inst-status-sample", "StatusSample", "")
        .await
        .unwrap();

    // Wait until terminal (Completed/Failed) or timeout.
    match client
        .wait_for_orchestration("inst-status-sample", std::time::Duration::from_secs(2))
        .await
        .unwrap()
    {
        OrchestrationStatus::Completed { output } => assert_eq!(output, "done"),
        OrchestrationStatus::Failed { details } => {
            panic!("unexpected failure: {}", details.display_message())
        }
        other => panic!("unexpected orchestration status: {:?}", other),
    }
    rt.shutdown(None).await;
}

// =============================================================================
// Ported from e2e_samples.rs - Nested Error Handling
// =============================================================================

/// Nested function with `?` operator error propagation
#[tokio::test]
async fn simplified_nested_function_error_handling() {
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;

    let activity_registry = ActivityRegistry::builder()
        .register("ProcessData", |_ctx: ActivityContext, input: String| async move {
            if input.contains("error") {
                Err("Processing failed".to_string())
            } else {
                Ok(format!("Processed: {input}"))
            }
        })
        .register("FormatOutput", |_ctx: ActivityContext, input: String| async move {
            Ok(format!("Final: {input}"))
        })
        .build();

    // Nested function that can fail with `?`
    async fn process_and_format(ctx: &OrchestrationContext, data: &str) -> Result<String, String> {
        let processed = ctx.simplified_schedule_activity("ProcessData", data.to_string()).await?;
        let formatted = ctx.simplified_schedule_activity("FormatOutput", processed).await?;
        Ok(formatted)
    }

    let orchestration = |ctx: OrchestrationContext, input: String| async move {
        let result = process_and_format(&ctx, &input).await?;
        Ok(result)
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("NestedErrorHandling", orchestration)
        .build();

    let options = RuntimeOptions {
        use_simplified_replay: true,
        orchestration_concurrency: 1,
        worker_concurrency: 1,
        ..Default::default()
    };

    let rt = runtime::Runtime::start_with_options(
        store.clone(),
        Arc::new(activity_registry),
        orchestration_registry,
        options,
    )
    .await;

    let client = Client::new(store.clone());

    // Test successful case
    client
        .start_orchestration("inst-nested-error-1", "NestedErrorHandling", "test")
        .await
        .unwrap();

    match client
        .wait_for_orchestration("inst-nested-error-1", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Completed { output } => {
            assert_eq!(output, "Final: Processed: test");
        }
        runtime::OrchestrationStatus::Failed { details } => {
            panic!("orchestration failed: {}", details.display_message())
        }
        other => panic!("unexpected orchestration status: {:?}", other),
    }

    // Test error case
    client
        .start_orchestration("inst-nested-error-2", "NestedErrorHandling", "error")
        .await
        .unwrap();

    match client
        .wait_for_orchestration("inst-nested-error-2", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Failed { details } => {
            assert!(details.display_message().contains("Processing failed"));
        }
        runtime::OrchestrationStatus::Completed { output } => {
            panic!("Expected failure but got success: {output}")
        }
        other => panic!("unexpected orchestration status: {:?}", other),
    }

    rt.shutdown(None).await;
}

// =============================================================================
// Ported from e2e_samples.rs - Error Recovery with Logging
// =============================================================================

/// Error recovery with logging: graceful failure handling
#[tokio::test]
async fn simplified_error_recovery_with_logging() {
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;

    let activity_registry = ActivityRegistry::builder()
        .register("ProcessData", |_ctx: ActivityContext, input: String| async move {
            if input.contains("error") {
                Err("Processing failed".to_string())
            } else {
                Ok(format!("Processed: {input}"))
            }
        })
        .register("LogError", |_ctx: ActivityContext, message: String| async move {
            Ok(format!("Logged: {message}"))
        })
        .register("DefaultValue", |_ctx: ActivityContext, _input: String| async move {
            Ok("default".to_string())
        })
        .build();

    let orchestration = |ctx: OrchestrationContext, input: String| async move {
        let result = ctx.simplified_schedule_activity("ProcessData", input).await;

        match result {
            Ok(processed) => Ok(processed),
            Err(error) => {
                // Log the error
                let _logged = ctx.simplified_schedule_activity("LogError", error).await?;
                // Return a default value instead of failing
                ctx.simplified_schedule_activity("DefaultValue", "").await
            }
        }
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("ErrorRecovery", orchestration)
        .build();

    let options = RuntimeOptions {
        use_simplified_replay: true,
        orchestration_concurrency: 1,
        worker_concurrency: 1,
        ..Default::default()
    };

    let rt = runtime::Runtime::start_with_options(
        store.clone(),
        Arc::new(activity_registry),
        orchestration_registry,
        options,
    )
    .await;

    let client = Client::new(store.clone());

    // Test successful case
    client
        .start_orchestration("error-recovery-1", "ErrorRecovery", "valid")
        .await
        .unwrap();

    match client
        .wait_for_orchestration("error-recovery-1", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Completed { output } => {
            assert_eq!(output, "Processed: valid");
        }
        runtime::OrchestrationStatus::Failed { details } => {
            panic!("orchestration failed: {}", details.display_message())
        }
        other => panic!("unexpected orchestration status: {:?}", other),
    }

    // Test recovery case (error is logged and default value returned)
    client
        .start_orchestration("error-recovery-2", "ErrorRecovery", "error")
        .await
        .unwrap();

    match client
        .wait_for_orchestration("error-recovery-2", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Completed { output } => {
            assert_eq!(output, "default");
        }
        runtime::OrchestrationStatus::Failed { details } => {
            panic!("orchestration should recover: {}", details.display_message())
        }
        other => panic!("unexpected orchestration status: {:?}", other),
    }

    rt.shutdown(None).await;
}

// =============================================================================
// Ported from e2e_samples.rs - Self-Pruning Eternal Orchestration
// =============================================================================

/// Self-Pruning Eternal Orchestration: uses continue_as_new and prunes old executions.
///
/// Pattern: An eternal orchestration that processes work in batches using ContinueAsNew,
/// and prunes its own old executions to bound storage growth.
#[tokio::test]
async fn simplified_self_pruning_eternal_orchestration() {
    use duroxide::providers::PruneOptions;
    use serde::{Deserialize, Serialize};

    let (store, _temp_dir) = common::create_sqlite_store_disk().await;

    // Track how many times we pruned
    let prune_count = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let prune_count_clone = prune_count.clone();

    let activity_registry = ActivityRegistry::builder()
        .register("ProcessBatch", |_ctx: ActivityContext, batch_num: String| async move {
            Ok(format!("Processed batch {batch_num}"))
        })
        .register("PruneSelf", move |ctx: ActivityContext, _input: String| {
            let prune_count = prune_count_clone.clone();
            async move {
                let client = ctx.get_client();
                let instance_id = ctx.instance_id().to_string();

                // Prune all but the current execution (keep_last: 1)
                let result = client
                    .prune_executions(
                        &instance_id,
                        PruneOptions {
                            keep_last: Some(1),
                            ..Default::default()
                        },
                    )
                    .await
                    .map_err(|e| e.to_string())?;

                prune_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);

                Ok(format!("Pruned {} executions", result.executions_deleted))
            }
        })
        .build();

    // Eternal orchestration that processes 5 batches then completes
    let orchestration = |ctx: OrchestrationContext, state_str: String| async move {
        #[derive(Serialize, Deserialize)]
        struct State {
            batch_num: u32,
            total_batches: u32,
        }

        let state: State = serde_json::from_str(&state_str).unwrap_or(State {
            batch_num: 0,
            total_batches: 5,
        });

        // Process current batch
        let _result = ctx
            .simplified_schedule_activity("ProcessBatch", state.batch_num.to_string())
            .await?;

        // Prune old executions (keep only current)
        let _prune_result = ctx
            .simplified_schedule_activity("PruneSelf", "")
            .await?;

        if state.batch_num >= state.total_batches - 1 {
            // Done processing all batches
            return Ok(format!("Completed {} batches", state.total_batches));
        }

        // Continue with next batch
        let next_state = State {
            batch_num: state.batch_num + 1,
            total_batches: state.total_batches,
        };
        ctx.continue_as_new(serde_json::to_string(&next_state).unwrap()).await
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("SelfPruningOrch", orchestration)
        .build();

    let options = RuntimeOptions {
        use_simplified_replay: true,
        orchestration_concurrency: 1,
        worker_concurrency: 1,
        ..Default::default()
    };

    let rt = runtime::Runtime::start_with_options(
        store.clone(),
        Arc::new(activity_registry),
        orchestration_registry,
        options,
    )
    .await;

    let client = Client::new(store.clone());

    // Start the self-pruning orchestration
    client
        .start_orchestration("inst-self-prune", "SelfPruningOrch", "{}")
        .await
        .unwrap();

    // Wait for completion
    match client
        .wait_for_orchestration("inst-self-prune", std::time::Duration::from_secs(30))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Completed { output } => {
            assert!(output.contains("Completed 5 batches"));
        }
        runtime::OrchestrationStatus::Failed { details } => {
            panic!("orchestration failed: {}", details.display_message())
        }
        other => panic!("unexpected orchestration status: {:?}", other),
    }

    // Verify pruning occurred
    let prunes = prune_count.load(std::sync::atomic::Ordering::SeqCst);
    assert!(prunes >= 4, "Should have pruned at least 4 times, got {prunes}");

    // Verify only 1 execution remains (the final one)
    let executions = client.list_executions("inst-self-prune").await.unwrap();
    assert_eq!(
        executions.len(),
        1,
        "Only final execution should remain after self-pruning"
    );

    rt.shutdown(None).await;
}

// =============================================================================
// Ported from e2e_samples.rs - Cancellation
// =============================================================================

/// Cancellation: parent cascades cancellation to children
///
/// This test verifies that when a parent orchestration is cancelled,
/// the cancellation propagates to its children.
#[tokio::test]
async fn simplified_cancellation_parent_cascades_to_children() {
    use duroxide::EventKind;

    let (store, _temp_dir) = common::create_sqlite_store_disk().await;

    let activity_registry = ActivityRegistry::builder().build();

    // Child: waits forever (until canceled)
    let child = |ctx: OrchestrationContext, _input: String| async move {
        let _ = ctx.simplified_schedule_wait("Go").await;
        Ok("done".to_string())
    };

    // Parent: starts child with explicit ID and awaits its completion
    let parent = |ctx: OrchestrationContext, _input: String| async move {
        let _ = ctx
            .simplified_schedule_sub_orchestration_with_id("ChildSample", Some("child-cancel-1"), "seed")
            .await?;
        Ok::<_, String>("parent_done".to_string())
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("ChildSample", child)
        .register("ParentSample", parent)
        .build();

    let options = RuntimeOptions {
        use_simplified_replay: true,
        orchestration_concurrency: 2,
        worker_concurrency: 1,
        dispatcher_min_poll_interval: std::time::Duration::from_millis(10),
        ..Default::default()
    };

    let rt = runtime::Runtime::start_with_options(
        store.clone(),
        Arc::new(activity_registry),
        orchestration_registry,
        options,
    )
    .await;

    let client = Client::new(store.clone());
    client
        .start_orchestration("inst-sample-cancel", "ParentSample", "")
        .await
        .unwrap();

    // Allow scheduling turn to run and child to start
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Cancel the parent
    let _ = client.cancel_instance("inst-sample-cancel", "user_request").await;

    // Wait for the parent to fail with cancelled error
    let ok = common::wait_for_history(
        store.clone(),
        "inst-sample-cancel",
        |hist| {
            hist.iter().rev().any(|e| {
                matches!(
                    &e.kind,
                    EventKind::OrchestrationFailed { details, .. } if matches!(
                        details,
                        duroxide::ErrorDetails::Application {
                            kind: duroxide::AppErrorKind::Cancelled { reason },
                            ..
                        } if reason == "user_request"
                    )
                )
            })
        },
        5_000,
    )
    .await;
    assert!(ok, "timeout waiting for parent cancel failure");

    // Verify child was also canceled (using explicit child ID)
    let child_id = "inst-sample-cancel::child-cancel-1";
    let ok_child = common::wait_for_history(
        store.clone(),
        child_id,
        |hist| {
            hist.iter()
                .any(|e| matches!(&e.kind, EventKind::OrchestrationCancelRequested { .. }))
                && hist.iter().any(|e| {
                    matches!(
                        &e.kind,
                        EventKind::OrchestrationFailed { details, .. } if matches!(
                            details,
                            duroxide::ErrorDetails::Application {
                                kind: duroxide::AppErrorKind::Cancelled { reason },
                                ..
                            } if reason == "parent canceled"
                        )
                    )
                })
        },
        5_000,
    )
    .await;
    assert!(ok_child, "timeout waiting for child cancel for {child_id}");

    rt.shutdown(None).await;
}

// =============================================================================
// Ported from e2e_samples.rs - Versioning
// =============================================================================

/// Versioning: start with latest vs exact version policy
///
/// Note: This test uses the orchestration registry versioning, not simplified scheduling.
/// The simplified runtime still honors the version policy.
#[tokio::test]
async fn simplified_versioning_start_latest_vs_exact() {
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;

    // Two versions: return a string indicating which version executed
    let v1 = |_: OrchestrationContext, _in: String| async move { Ok("v1".to_string()) };
    let v2 = |_: OrchestrationContext, _in: String| async move { Ok("v2".to_string()) };

    let reg = OrchestrationRegistry::builder()
        // Default registration is 1.0.0
        .register("Versioned", v1)
        // Add a later version 2.0.0
        .register_versioned("Versioned", "2.0.0", v2)
        .build();
    let acts = ActivityRegistry::builder().build();

    let options = RuntimeOptions {
        use_simplified_replay: true,
        orchestration_concurrency: 1,
        worker_concurrency: 1,
        ..Default::default()
    };

    let rt = runtime::Runtime::start_with_options(store.clone(), Arc::new(acts), reg.clone(), options).await;

    // With default policy (Latest), a new start should run v2
    let client = Client::new(store.clone());
    client
        .start_orchestration("inst-vers-latest", "Versioned", "")
        .await
        .unwrap();

    match client
        .wait_for_orchestration("inst-vers-latest", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Completed { output } => assert_eq!(output, "v2"),
        runtime::OrchestrationStatus::Failed { details } => {
            panic!("orchestration failed: {}", details.display_message())
        }
        other => panic!("unexpected orchestration status: {:?}", other),
    }

    // Pin new starts to 1.0.0 via policy, verify it runs v1
    reg.set_version_policy(
        "Versioned",
        duroxide::runtime::VersionPolicy::Exact(semver::Version::parse("1.0.0").unwrap()),
    );
    client
        .start_orchestration("inst-vers-exact", "Versioned", "")
        .await
        .unwrap();

    match client
        .wait_for_orchestration("inst-vers-exact", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Completed { output } => assert_eq!(output, "v1"),
        runtime::OrchestrationStatus::Failed { details } => {
            panic!("orchestration failed: {}", details.display_message())
        }
        other => panic!("unexpected orchestration status: {:?}", other),
    }

    rt.shutdown(None).await;
}
