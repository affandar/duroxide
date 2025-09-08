use duroxide::providers::Provider;
use duroxide::providers::sqlite::SqliteProvider;
use duroxide::runtime::registry::ActivityRegistry;
use duroxide::runtime::{self};
use duroxide::{Event, OrchestrationContext, OrchestrationRegistry};
use std::sync::Arc as StdArc;
use tempfile::TempDir;

mod common;

/// Helper to create a SQLite store for testing
async fn create_sqlite_store(name: &str) -> (StdArc<dyn Provider>, TempDir, String) {
    let td = tempfile::tempdir().unwrap();
    let db_path = td.path().join(format!("{}.db", name));
    std::fs::File::create(&db_path).unwrap();
    let db_url = format!("sqlite:{}", db_path.display());
    let store = StdArc::new(SqliteProvider::new(&db_url).await.unwrap()) as StdArc<dyn Provider>;
    (store, td, db_url)
}

/// Test that verifies timer recovery after crash between dequeue and fire
/// 
/// Scenario:
/// 1. Orchestration schedules a timer
/// 2. Timer is dequeued from the timer queue  
/// 3. System crashes before timer fires (before TimerFired is enqueued)
/// 4. System restarts
/// 5. Timer should be redelivered and fire correctly
#[tokio::test]
async fn timer_recovery_after_crash_before_fire() {
    let (store1, _td, db_url) = create_sqlite_store("timer_recovery").await;

    const TIMER_MS: u64 = 500;
    
    // Simple orchestration that schedules a timer and then completes
    let orch = |ctx: OrchestrationContext, _input: String| async move {
        // Schedule a timer with enough delay that we can "crash" before it fires
        ctx.schedule_timer(TIMER_MS).into_timer().await;
        
        // Do something after timer to prove it fired
        let result = ctx.schedule_activity("PostTimer", "done").into_activity().await?;
        Ok(result)
    };

    let activity_registry = ActivityRegistry::builder()
        .register("PostTimer", |input: String| async move {
            Ok(format!("Timer fired, then: {}", input))
        })
        .build();

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("TimerRecoveryTest", orch)
        .build();

    // Phase 1: Start orchestration and wait for timer to be scheduled
    let rt1 = runtime::Runtime::start_with_store(
        store1.clone(),
        StdArc::new(activity_registry.clone()),
        orchestration_registry.clone(),
    )
    .await;

    let instance = "inst-timer-recovery";
    let client1 = duroxide::Client::new(store1.clone());
    let _ = client1.start_orchestration(instance, "TimerRecoveryTest", "").await.unwrap();

    // Wait for timer to be created
    assert!(
        common::wait_for_history(
            store1.clone(),
            instance,
            |h| h.iter().any(|e| matches!(e, Event::TimerCreated { .. })),
            2_000
        )
        .await,
        "Timer should be created"
    );

    // Verify timer hasn't fired yet
    let hist_before = store1.read(instance).await;
    assert!(
        !hist_before.iter().any(|e| matches!(e, Event::TimerFired { .. })),
        "Timer should not have fired yet"
    );
    
    // Extract timer details for verification
    let (timer_id, fire_at_ms) = hist_before
        .iter()
        .find_map(|e| match e {
            Event::TimerCreated { id, fire_at_ms, .. } => Some((*id, *fire_at_ms)),
            _ => None,
        })
        .expect("Timer created event should exist");

    // Simulate crash by shutting down runtime
    println!("Simulating crash - shutting down runtime before timer fires...");
    rt1.shutdown().await;

    // Small delay to ensure shutdown completes
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Phase 2: "Restart" system with new runtime but same store
    println!("Restarting system...");
    let store2 = StdArc::new(SqliteProvider::new(&db_url).await.unwrap()) as StdArc<dyn Provider>;
    let rt2 = runtime::Runtime::start_with_store(
        store2.clone(),
        StdArc::new(activity_registry),
        orchestration_registry,
    )
    .await;

    // The runtime should automatically resume the orchestration and reprocess pending timers
    
    // Wait for timer to be processed - may take a moment for timer dispatcher to start
    // First wait for the timer to fire
    let timer_fired = common::wait_for_history(
        store2.clone(),
        instance,
        |h| h.iter().any(|e| matches!(e, Event::TimerFired { id, .. } if *id == timer_id)),
        3_000  // Give timer dispatcher a few seconds to process
    )
    .await;
    
    if !timer_fired {
        // If timer hasn't fired yet, give it more time
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
    
    // Now wait for orchestration to complete
    let client2 = duroxide::Client::new(store2.clone());
    match client2
        .wait_for_orchestration(instance, std::time::Duration::from_secs(10))
        .await
        .unwrap()
    {
        duroxide::OrchestrationStatus::Completed { output } => {
            assert_eq!(output, "Timer fired, then: done");
            println!("✅ Orchestration completed successfully after restart");
        }
        duroxide::OrchestrationStatus::Failed { error } => {
            panic!("Orchestration failed after restart: {}", error);
        }
        status => {
            panic!("Unexpected orchestration status after restart: {:?}", status);
        }
    }

    // Verify the timer actually fired
    // Sometimes there's a race where the orchestration completes before the TimerFired 
    // event is written to history, so give it a moment
    let timer_in_history = common::wait_for_history(
        store2.clone(),
        instance,
        |h| h.iter().any(|e| matches!(e, Event::TimerFired { id, .. } 
                                    if *id == timer_id)),
        3_000  // Wait a few seconds for the event to be written
    )
    .await;
    
    let hist_after = store2.read(instance).await;
    
    // Should have exactly one TimerCreated and one TimerFired
    let timer_created_count = hist_after
        .iter()
        .filter(|e| matches!(e, Event::TimerCreated { id, .. } if *id == timer_id))
        .count();
    let timer_fired_count = hist_after
        .iter()
        .filter(|e| matches!(e, Event::TimerFired { id, .. } 
                           if *id == timer_id))
        .count();
    
    // Debug output if timer didn't fire
    if timer_fired_count == 0 {
        println!("Timer did not fire. History events:");
        for event in &hist_after {
            println!("  {:?}", event);
        }
        println!("Expected timer_id: {}, fire_at_ms: {}", timer_id, fire_at_ms);
        println!("Timer wait result: {}", timer_in_history);
    }
    
    assert_eq!(timer_created_count, 1, "Should have exactly one TimerCreated event");
    assert_eq!(timer_fired_count, 1, "Should have exactly one TimerFired event");
    
    // Should have the activity that runs after timer
    assert!(
        hist_after
            .iter()
            .any(|e| matches!(e, Event::ActivityCompleted { result, .. } 
                            if result == "Timer fired, then: done")),
        "Post-timer activity should have completed"
    );

    println!("✅ Timer recovery test passed - timer fired correctly after restart");
    rt2.shutdown().await;
}

/// Test multiple timers with crash/recovery
#[tokio::test] 
async fn multiple_timers_recovery_after_crash() {
    let (store1, _td, db_url) = create_sqlite_store("multi_timer_recovery").await;

    const TIMER1_MS: u64 = 300;
    const TIMER2_MS: u64 = 600;
    const TIMER3_MS: u64 = 900;
    
    // Orchestration with multiple timers of different delays
    let orch = |ctx: OrchestrationContext, _input: String| async move {
        // Schedule three timers with different delays
        let t1 = ctx.schedule_timer(TIMER1_MS);
        let t2 = ctx.schedule_timer(TIMER2_MS);
        let t3 = ctx.schedule_timer(TIMER3_MS);
        
        // Wait for all timers
        ctx.join(vec![t1, t2, t3]).await;
        
        Ok("All timers fired".to_string())
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("MultiTimerTest", orch)
        .build();

    let activity_registry = ActivityRegistry::builder().build();

    // Phase 1: Start and wait for all timers to be created
    let rt1 = runtime::Runtime::start_with_store(
        store1.clone(),
        StdArc::new(activity_registry.clone()),
        orchestration_registry.clone(),
    )
    .await;

    let instance = "inst-multi-timer-recovery";
    let client1 = duroxide::Client::new(store1.clone());
    let _ = client1.start_orchestration(instance, "MultiTimerTest", "").await.unwrap();

    // Wait for all 3 timers to be created
    assert!(
        common::wait_for_history(
            store1.clone(),
            instance,
            |h| h.iter().filter(|e| matches!(e, Event::TimerCreated { .. })).count() >= 3,
            2_000
        )
        .await,
        "All 3 timers should be created"
    );

    // Crash before any timer fires
    let hist_before = store1.read(instance).await;
    assert_eq!(
        hist_before
            .iter()
            .filter(|e| matches!(e, Event::TimerFired { .. }))
            .count(),
        0,
        "No timers should have fired yet"
    );

    println!("Crashing with 3 pending timers...");
    rt1.shutdown().await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Phase 2: Restart and verify all timers fire
    println!("Restarting...");
    let store2 = StdArc::new(SqliteProvider::new(&db_url).await.unwrap()) as StdArc<dyn Provider>;
    let rt2 = runtime::Runtime::start_with_store(
        store2.clone(),
        StdArc::new(activity_registry),
        orchestration_registry,
    )
    .await;

    // Wait for completion
    let client2 = duroxide::Client::new(store2.clone());
    match client2
        .wait_for_orchestration(instance, std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        duroxide::OrchestrationStatus::Completed { output } => {
            assert_eq!(output, "All timers fired");
            println!("✅ All timers fired after recovery");
        }
        duroxide::OrchestrationStatus::Failed { error } => {
            panic!("Orchestration failed: {}", error);
        }
        status => {
            panic!("Unexpected status: {:?}", status);
        }
    }

    // Verify all 3 timers fired
    let hist_after = store2.read(instance).await;
    let timer_fired_count = hist_after
        .iter()
        .filter(|e| matches!(e, Event::TimerFired { .. }))
        .count();
    
    assert_eq!(timer_fired_count, 3, "All 3 timers should have fired");

    rt2.shutdown().await;
}
