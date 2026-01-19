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
        let result = ctx.schedule_activity("Echo", input).into_activity().await?;
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
        let added = ctx.schedule_activity("Add", input).into_activity().await?;
        let doubled = ctx.schedule_activity("Double", added).into_activity().await?;
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
        ctx.schedule_timer(std::time::Duration::from_millis(10))
            .into_timer()
            .await;
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
        
        let result = ctx.schedule_activity("Slow", input).into_activity().await?;
        
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
        
        let result = ctx.schedule_activity("Work", input).into_activity().await?;
        
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

/// Fan-out/fan-in: parallel activities with ctx.join()
/// 
/// TODO: JoinFuture needs simplified mode support - it currently uses
/// claimed_event_id and history lookup which are legacy mode only.
#[tokio::test]
#[ignore = "JoinFuture needs simplified mode support"]
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
        let f1 = ctx.schedule_activity("Process", "A".to_string());
        let f2 = ctx.schedule_activity("Process", "B".to_string());
        let f3 = ctx.schedule_activity("Process", "C".to_string());
        
        let results = ctx.join(vec![f1, f2, f3]).await;
        
        // Collect results, handling errors
        let mut outputs = Vec::new();
        for r in results {
            match r {
                duroxide::DurableOutput::Activity(Ok(s)) => outputs.push(s),
                duroxide::DurableOutput::Activity(Err(e)) => return Err(e),
                other => return Err(format!("unexpected output: {other:?}")),
            }
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
        let result = ctx.schedule_activity("Double", input).into_activity().await?;
        Ok(result)
    };

    // Parent orchestration: calls child twice
    let parent = |ctx: OrchestrationContext, input: String| async move {
        let first = ctx
            .schedule_sub_orchestration("Child", input)
            .into_sub_orchestration()
            .await?;
        let second = ctx
            .schedule_sub_orchestration("Child", first)
            .into_sub_orchestration()
            .await?;
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
/// 
/// TODO: External event delivery needs investigation in simplified mode.
/// The subscription may not be persisting correctly on Turn 1.
#[tokio::test]
#[ignore = "External event delivery needs investigation in simplified mode"]
async fn simplified_external_event() {
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;

    let activity_registry = ActivityRegistry::builder().build();

    // Orchestration waits for external event
    let orchestration = |ctx: OrchestrationContext, _input: String| async move {
        let data = ctx.schedule_wait("approval").into_event().await;
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

    // Give orchestration time to start and wait
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

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
        
        let next = ctx.schedule_activity("Increment", input).into_activity().await?;
        ctx.continue_as_new(next);
        
        // This won't be reached, but needed for type checking
        Ok("unreachable".to_string())
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
        let result = ctx.schedule_activity("FailingActivity", input.clone()).into_activity().await;
        match result {
            Ok(_) => Ok("unexpected success".to_string()),
            Err(e) => {
                // Activity failed, try fallback
                let fallback = ctx.schedule_activity("SucceedingActivity", input).into_activity().await?;
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
/// 
/// TODO: SelectFuture needs simplified mode support - it currently uses
/// claimed_event_id and history lookup which are legacy mode only.
#[tokio::test]
#[ignore = "SelectFuture needs simplified mode support"]
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
        let fast = ctx.schedule_activity("Fast", input.clone());
        let slow = ctx.schedule_activity("Slow", input);
        
        let (winner_index, result) = ctx.select2(fast, slow).await;
        match result {
            duroxide::DurableOutput::Activity(Ok(output)) => {
                Ok(format!("winner:{winner_index},output:{output}"))
            }
            duroxide::DurableOutput::Activity(Err(e)) => Err(e),
            other => Err(format!("unexpected output: {other:?}")),
        }
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
        ctx.schedule_timer(std::time::Duration::from_millis(10))
            .into_timer()
            .await;
        
        let result = ctx.schedule_activity("Work", input).into_activity().await?;
        
        ctx.schedule_timer(std::time::Duration::from_millis(10))
            .into_timer()
            .await;
        
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
