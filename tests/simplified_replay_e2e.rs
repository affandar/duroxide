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
