// Use SQLite via common helper
// Tests for v2 futures API select/join behavior using standard futures combinators
use duroxide::runtime::registry::ActivityRegistry;
use duroxide::runtime::{self, OrchestrationStatus};
use duroxide::{ActivityContext, EventKind, OrchestrationContext, OrchestrationRegistry};
use std::sync::Arc as StdArc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

mod common;

#[tokio::test]
async fn select_two_externals_declaration_order_wins() {
    let (store, _td) = common::create_sqlite_store_disk().await;

    let orchestrator = |ctx: OrchestrationContext, _input: String| async move {
        ctx.initialize_v2();
        let mut a = std::pin::pin!(ctx.schedule_wait_v2("A"));
        let mut b = std::pin::pin!(ctx.schedule_wait_v2("B"));
        // Use select_biased! for deterministic declaration-order polling
        futures::select_biased! {
            v = a => Ok(format!("A:{v}")),
            v = b => Ok(format!("B:{v}")),
        }
    };

    let acts = ActivityRegistry::builder().build();
    let reg = OrchestrationRegistry::builder()
        .register("ABSelect2", orchestrator)
        .build();
    let rt1 = runtime::Runtime::start_with_store(store.clone(), StdArc::new(acts), reg).await;
    let client = duroxide::Client::new(store.clone());

    client.start_orchestration("inst-ab2", "ABSelect2", "").await.unwrap();

    assert!(
        common::wait_for_history(
            store.clone(),
            "inst-ab2",
            |h| {
                let mut seen_a = false;
                let mut seen_b = false;
                for e in h.iter() {
                    if let EventKind::ExternalSubscribed { name } = &e.kind {
                        if name == "A" {
                            seen_a = true;
                        }
                        if name == "B" {
                            seen_b = true;
                        }
                    }
                }
                seen_a && seen_b
            },
            3_000
        )
        .await,
        "timeout waiting for subscriptions"
    );
    rt1.shutdown(None).await;

    let wi_b = duroxide::providers::WorkItem::ExternalRaised {
        instance: "inst-ab2".to_string(),
        name: "B".to_string(),
        data: "vb".to_string(),
    };
    let wi_a = duroxide::providers::WorkItem::ExternalRaised {
        instance: "inst-ab2".to_string(),
        name: "A".to_string(),
        data: "va".to_string(),
    };
    let _ = store.enqueue_for_orchestrator(wi_b, None).await;
    let _ = store.enqueue_for_orchestrator(wi_a, None).await;

    let acts2 = ActivityRegistry::builder().build();
    let reg2 = OrchestrationRegistry::builder()
        .register("ABSelect2", orchestrator)
        .build();
    let rt2 = runtime::Runtime::start_with_store(store.clone(), StdArc::new(acts2), reg2).await;

    assert!(
        common::wait_for_history(
            store.clone(),
            "inst-ab2",
            |h| {
                h.iter()
                    .any(|e| matches!(&e.kind, EventKind::OrchestrationCompleted { .. }))
            },
            5_000
        )
        .await,
        "timeout waiting for completion"
    );
    let hist = store.read("inst-ab2").await.unwrap_or_default();
    let output = match hist.last().map(|e| &e.kind) {
        Some(EventKind::OrchestrationCompleted { output }) => output.clone(),
        _ => String::new(),
    };

    // With v2 futures::select!, during replay the future whose completion
    // appears first in history wins. Since B was enqueued before A, B wins.
    let b_index = hist
        .iter()
        .position(|e| matches!(&e.kind, EventKind::ExternalEvent { name, .. } if name == "B"));
    let a_index = hist
        .iter()
        .position(|e| matches!(&e.kind, EventKind::ExternalEvent { name, .. } if name == "A"));

    assert!(b_index.is_some(), "expected ExternalEvent B in history: {hist:#?}");
    assert!(a_index.is_some(), "expected ExternalEvent A in history: {hist:#?}");

    // The key assertion: select picks A (first in declaration order) when both ready at once
    assert_eq!(
        output, "A:va",
        "expected A to win since it's first in declaration order, got {output}"
    );
    rt2.shutdown(None).await;
}

#[tokio::test]
async fn select_two_externals_declaration_order_wins_variant() {
    let (store, _td) = common::create_sqlite_store_disk().await;

    let orchestrator = |ctx: OrchestrationContext, _input: String| async move {
        ctx.initialize_v2();
        let mut a = std::pin::pin!(ctx.schedule_wait_v2("A"));
        let mut b = std::pin::pin!(ctx.schedule_wait_v2("B"));
        // Use select_biased! for deterministic declaration-order polling
        futures::select_biased! {
            v = a => Ok(format!("A:{v}")),
            v = b => Ok(format!("B:{v}")),
        }
    };

    let acts = ActivityRegistry::builder().build();
    let reg = OrchestrationRegistry::builder()
        .register("ABSelect", orchestrator)
        .build();
    let rt1 = runtime::Runtime::start_with_store(store.clone(), StdArc::new(acts), reg).await;
    let client = duroxide::Client::new(store.clone());

    client.start_orchestration("inst-ab", "ABSelect", "").await.unwrap();

    assert!(
        common::wait_for_history(
            store.clone(),
            "inst-ab",
            |h| {
                let mut seen_a = false;
                let mut seen_b = false;
                for e in h.iter() {
                    if let EventKind::ExternalSubscribed { name } = &e.kind {
                        if name == "A" {
                            seen_a = true;
                        }
                        if name == "B" {
                            seen_b = true;
                        }
                    }
                }
                seen_a && seen_b
            },
            3_000
        )
        .await,
        "timeout waiting for subscriptions"
    );
    rt1.shutdown(None).await;

    let wi_b = duroxide::providers::WorkItem::ExternalRaised {
        instance: "inst-ab".to_string(),
        name: "B".to_string(),
        data: "vb".to_string(),
    };
    let wi_a = duroxide::providers::WorkItem::ExternalRaised {
        instance: "inst-ab".to_string(),
        name: "A".to_string(),
        data: "va".to_string(),
    };
    let _ = store.enqueue_for_orchestrator(wi_b, None).await;
    let _ = store.enqueue_for_orchestrator(wi_a, None).await;

    let acts2 = ActivityRegistry::builder().build();
    let reg2 = OrchestrationRegistry::builder()
        .register("ABSelect", orchestrator)
        .build();
    let rt2 = runtime::Runtime::start_with_store(store.clone(), StdArc::new(acts2), reg2).await;

    assert!(
        common::wait_for_history(
            store.clone(),
            "inst-ab",
            |h| {
                h.iter()
                    .any(|e| matches!(&e.kind, EventKind::OrchestrationCompleted { .. }))
            },
            5_000
        )
        .await,
        "timeout waiting for completion"
    );
    let hist = store.read("inst-ab").await.unwrap_or_default();
    let output = match hist.last().map(|e| &e.kind) {
        Some(EventKind::OrchestrationCompleted { output }) => output.clone(),
        _ => String::new(),
    };

    // With v2 futures::select!, during replay the future whose completion
    // appears first in history wins. Since B was enqueued before A, B wins.
    let b_index = hist
        .iter()
        .position(|e| matches!(&e.kind, EventKind::ExternalEvent { name, .. } if name == "B"));
    let a_index = hist
        .iter()
        .position(|e| matches!(&e.kind, EventKind::ExternalEvent { name, .. } if name == "A"));

    assert!(b_index.is_some(), "expected ExternalEvent B in history: {hist:#?}");
    assert!(a_index.is_some(), "expected ExternalEvent A in history: {hist:#?}");

    // The key assertion: select picks A (first in declaration order) when both ready at once
    assert_eq!(
        output, "A:va",
        "expected A to win since it's first in declaration order, got {output}"
    );
    rt2.shutdown(None).await;
}

#[tokio::test]
async fn select_three_mixed_declaration_order_wins() {
    // A (external), T (timer), B (external): enqueue B first, then A; timer much later
    let (store, _td) = common::create_sqlite_store_disk().await;

    let orchestrator = |ctx: OrchestrationContext, _input: String| async move {
        ctx.initialize_v2();
        let mut a = std::pin::pin!(ctx.schedule_wait_v2("A"));
        let mut t = std::pin::pin!(ctx.schedule_timer_v2(Duration::from_millis(500)));
        let mut b = std::pin::pin!(ctx.schedule_wait_v2("B"));
        // Use select_biased! for deterministic declaration-order polling
        futures::select_biased! {
            v = a => Ok(format!("A:{v}")),
            _ = t => Ok("T".to_string()),
            v = b => Ok(format!("B:{v}")),
        }
    };

    let acts = ActivityRegistry::builder().build();
    let reg = OrchestrationRegistry::builder()
        .register("ATBSelect", orchestrator)
        .build();
    let rt1 = runtime::Runtime::start_with_store(store.clone(), StdArc::new(acts), reg).await;
    let client = duroxide::Client::new(store.clone());

    client.start_orchestration("inst-atb", "ATBSelect", "").await.unwrap();
    assert!(
        common::wait_for_history(
            store.clone(),
            "inst-atb",
            |h| {
                let mut seen_a = false;
                let mut seen_b = false;
                for e in h.iter() {
                    if let EventKind::ExternalSubscribed { name } = &e.kind {
                        if name == "A" {
                            seen_a = true;
                        }
                        if name == "B" {
                            seen_b = true;
                        }
                    }
                }
                seen_a && seen_b
            },
            10_000
        )
        .await
    );

    // TIMING-SENSITIVE: Use immediate shutdown (no graceful wait) because:
    // - Timer(500ms) is ticking and will fire during rt2 startup if we delay
    // - Graceful shutdown would add 1000ms delay, virtually guaranteeing timer fires first
    // - Test expects externals to be processed before timer expires
    // - Immediate abort stops timer dispatcher instantly, preventing premature firing
    rt1.shutdown(Some(0)).await;

    let wi_b = duroxide::providers::WorkItem::ExternalRaised {
        instance: "inst-atb".to_string(),
        name: "B".to_string(),
        data: "vb".to_string(),
    };
    let wi_a = duroxide::providers::WorkItem::ExternalRaised {
        instance: "inst-atb".to_string(),
        name: "A".to_string(),
        data: "va".to_string(),
    };
    let _ = store.enqueue_for_orchestrator(wi_b, None).await;
    let _ = store.enqueue_for_orchestrator(wi_a, None).await;

    let acts2 = ActivityRegistry::builder().build();
    let reg2 = OrchestrationRegistry::builder()
        .register("ATBSelect", orchestrator)
        .build();
    let rt2 = runtime::Runtime::start_with_store(store.clone(), StdArc::new(acts2), reg2).await;

    assert!(
        common::wait_for_history(
            store.clone(),
            "inst-atb",
            |h| {
                h.iter()
                    .any(|e| matches!(&e.kind, EventKind::OrchestrationCompleted { .. }))
            },
            5_000
        )
        .await
    );
    let hist = store.read("inst-atb").await.unwrap_or_default();
    let output = match hist.last().map(|e| &e.kind) {
        Some(EventKind::OrchestrationCompleted { output }) => output.clone(),
        _ => String::new(),
    };

    // With v2 futures::select!, during replay the future whose completion
    // appears first in history wins. Since B was enqueued before A, B wins.
    let b_index = hist
        .iter()
        .position(|e| matches!(&e.kind, EventKind::ExternalEvent { name, .. } if name == "B"));
    let a_index = hist
        .iter()
        .position(|e| matches!(&e.kind, EventKind::ExternalEvent { name, .. } if name == "A"));

    assert!(b_index.is_some(), "expected ExternalEvent B in history: {hist:#?}");
    assert!(a_index.is_some(), "expected ExternalEvent A in history: {hist:#?}");

    // The key assertion: select picks A (first in declaration order) when both ready at once
    assert_eq!(
        output, "A:va",
        "expected A to win since it's first in declaration order, got {output}"
    );
    rt2.shutdown(None).await;
}

/// Test: When futures complete one-by-one across multiple turns (not all ready at once),
/// the join still returns results in declaration order because futures::join! 
/// always returns in declaration order regardless of completion order.
#[tokio::test]
async fn join_one_by_one_still_declaration_order() {
    let (store, _td) = common::create_sqlite_store_disk().await;

    let orchestrator = |ctx: OrchestrationContext, _input: String| async move {
        ctx.initialize_v2();
        // Declaration order: A first, then B
        let (a_val, b_val) = futures::join!(
            ctx.schedule_wait_v2("A"),
            ctx.schedule_wait_v2("B")
        );
        Ok(format!("{a_val},{b_val}"))
    };

    let acts = ActivityRegistry::builder().build();
    let reg = OrchestrationRegistry::builder()
        .register("JoinOneByOne", orchestrator)
        .build();
    let rt = runtime::Runtime::start_with_store(store.clone(), StdArc::new(acts), reg).await;
    let client = duroxide::Client::new(store.clone());

    client.start_orchestration("inst-join-seq", "JoinOneByOne", "").await.unwrap();

    // Wait for both subscriptions
    assert!(
        common::wait_for_history(
            store.clone(),
            "inst-join-seq",
            |h| {
                let a_sub = h.iter().any(|e| matches!(&e.kind, EventKind::ExternalSubscribed { name } if name == "A"));
                let b_sub = h.iter().any(|e| matches!(&e.kind, EventKind::ExternalSubscribed { name } if name == "B"));
                a_sub && b_sub
            },
            5_000
        )
        .await
    );

    // Send B first (inverse declaration order) - this completes B future first
    client.raise_event("inst-join-seq", "B", "vb").await.unwrap();
    
    // Wait for B to be processed before sending A
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    
    // Now send A - this completes A future second
    client.raise_event("inst-join-seq", "A", "va").await.unwrap();

    // Wait for completion
    assert!(
        common::wait_for_history(
            store.clone(),
            "inst-join-seq",
            |h| {
                h.iter()
                    .any(|e| matches!(&e.kind, EventKind::OrchestrationCompleted { .. }))
            },
            5_000
        )
        .await
    );

    let hist = store.read("inst-join-seq").await.unwrap_or_default();
    let output = match hist.last().map(|e| &e.kind) {
        Some(EventKind::OrchestrationCompleted { output }) => output.clone(),
        _ => String::new(),
    };
    
    // Even though B completed first (history order: B, A), futures::join! returns
    // results in declaration order (A, B), so output is "va,vb"
    assert_eq!(
        output, "va,vb",
        "futures::join! should return declaration order (A,B) even when B completed first"
    );
    
    rt.shutdown(None).await;
}

#[tokio::test]
async fn join_returns_declaration_order() {
    let (store, _td) = common::create_sqlite_store_disk().await;

    let orchestrator = |ctx: OrchestrationContext, _input: String| async move {
        ctx.initialize_v2();
        // With v2, use futures::join! to wait for both
        let (a_val, b_val) = futures::join!(
            ctx.schedule_wait_v2("A"),
            ctx.schedule_wait_v2("B")
        );
        // With v2, join returns results in declaration order, not history order
        Ok(format!("{a_val},{b_val}"))
    };

    let acts = ActivityRegistry::builder().build();
    let reg = OrchestrationRegistry::builder()
        .register("JoinAB", orchestrator)
        .build();
    let rt1 = runtime::Runtime::start_with_store(store.clone(), StdArc::new(acts), reg).await;
    let client = duroxide::Client::new(store.clone());

    client.start_orchestration("inst-join", "JoinAB", "").await.unwrap();
    assert!(
        common::wait_for_history(
            store.clone(),
            "inst-join",
            |h| {
                let mut seen_a = false;
                let mut seen_b = false;
                for e in h.iter() {
                    if let EventKind::ExternalSubscribed { name } = &e.kind {
                        if name == "A" {
                            seen_a = true;
                        }
                        if name == "B" {
                            seen_b = true;
                        }
                    }
                }
                seen_a && seen_b
            },
            10_000
        )
        .await
    );
    rt1.shutdown(None).await;

    // Enqueue B then A so history order is B, then A
    let wi_b = duroxide::providers::WorkItem::ExternalRaised {
        instance: "inst-join".to_string(),
        name: "B".to_string(),
        data: "vb".to_string(),
    };
    let wi_a = duroxide::providers::WorkItem::ExternalRaised {
        instance: "inst-join".to_string(),
        name: "A".to_string(),
        data: "va".to_string(),
    };
    let _ = store.enqueue_for_orchestrator(wi_b, None).await;
    let _ = store.enqueue_for_orchestrator(wi_a, None).await;

    let acts2 = ActivityRegistry::builder().build();
    let reg2 = OrchestrationRegistry::builder()
        .register("JoinAB", orchestrator)
        .build();
    let rt2 = runtime::Runtime::start_with_store(store.clone(), StdArc::new(acts2), reg2).await;

    assert!(
        common::wait_for_history(
            store.clone(),
            "inst-join",
            |h| {
                h.iter()
                    .any(|e| matches!(&e.kind, EventKind::OrchestrationCompleted { .. }))
            },
            5_000
        )
        .await
    );
    let hist = store.read("inst-join").await.unwrap_or_default();
    let output = match hist.last().map(|e| &e.kind) {
        Some(EventKind::OrchestrationCompleted { output }) => output.clone(),
        _ => String::new(),
    };
    // With v2 API, futures::join! returns results in declaration order (A, B)
    // not history order, so output is va,vb
    assert_eq!(output, "va,vb");
    rt2.shutdown(None).await;
}

// ============================================================================
// select2 Scheduling Event Consumption Tests (Regression)
// ============================================================================
//
// These tests verify the fix for a nondeterminism bug where select2 wouldn't
// consume the loser's scheduling events during replay.
//
// Original Bug: During replay, select2 would return immediately when the winner
// was found, leaving the loser's scheduling event (e.g., TimerCreated) unclaimed.
// When subsequent code tried to schedule new operations, it would see the
// unclaimed event and report a nondeterminism error.
//
// Fix: Modified AggregateDurableFuture::poll for AggregateMode::Select to use
// two-phase polling: first poll ALL children to ensure they claim their
// scheduling events, then check which one is ready.

/// Regression test: select2 loser's event must be consumed during replay
///
/// Previously, select2 would return immediately when the winner was found,
/// leaving the loser's scheduling event unclaimed. This caused nondeterminism
/// when subsequent code tried to schedule new operations.
///
/// Fixed by polling ALL children before checking for a winner.
#[tokio::test]
async fn test_select2_loser_event_consumed_during_replay() {
    let (store, _td) = common::create_sqlite_store_disk().await;
    let attempt_counter = StdArc::new(AtomicU32::new(0));
    let counter_clone = attempt_counter.clone();

    let activities = ActivityRegistry::builder()
        .register("FastFailActivity", move |_ctx: ActivityContext, _input: String| {
            let counter = counter_clone.clone();
            async move {
                let attempt = counter.fetch_add(1, Ordering::SeqCst) + 1;
                // Activity completes FAST with error - beats the 500ms timer
                Err(format!("fast failure on attempt {attempt}"))
            }
        })
        .build();

    let orchestrations = OrchestrationRegistry::builder()
        .register(
            "SelectLoserOrch",
            |ctx: OrchestrationContext, _input: String| async move {
                ctx.initialize_v2();
                // ATTEMPT 1: Race activity vs timer
                // Activity will complete fast (with error), timer (500ms) loses
                let mut timer1 = std::pin::pin!(ctx.schedule_timer_v2(Duration::from_millis(500)));
                let mut activity1 = std::pin::pin!(ctx.schedule_activity_v2("FastFailActivity", ""));
                
                // Activity wins (it's faster)
                let first_error = futures::select! {
                    result = activity1 => match result {
                        Err(e) => e,
                        Ok(_) => return Ok("unexpected success".to_string()),
                    },
                    _ = timer1 => return Err("timer won unexpectedly".to_string()),
                };

                // ATTEMPT 2: Schedule another activity
                // Previously this would fail with nondeterminism during replay
                // because the timer's scheduling event wasn't consumed
                let second_result = ctx.schedule_activity_v2("FastFailActivity", "").await;

                Ok(format!("first: {first_error}, second: {second_result:?}"))
            },
        )
        .build();

    let rt = runtime::Runtime::start_with_store(store.clone(), StdArc::new(activities), orchestrations).await;
    let client = duroxide::Client::new(store.clone());

    client
        .start_orchestration("select-loser-1", "SelectLoserOrch", "")
        .await
        .unwrap();

    match client
        .wait_for_orchestration("select-loser-1", Duration::from_secs(10))
        .await
        .unwrap()
    {
        OrchestrationStatus::Completed { output } => {
            // Should complete successfully now that the bug is fixed
            assert!(
                output.contains("first:"),
                "expected successful completion, got: {output}"
            );
        }
        OrchestrationStatus::Failed { details } => {
            let msg = details.display_message();
            panic!("should not fail with nondeterminism anymore: {msg}");
        }
        other => panic!("unexpected status: {other:?}"),
    }

    // Both activities should have been called
    assert_eq!(attempt_counter.load(Ordering::SeqCst), 2);

    // Wait for the loser timer to fire (it's 500ms, so wait a bit)
    tokio::time::sleep(Duration::from_millis(600)).await;

    // Check history: the loser timer's completion event (TimerFired) should be
    // properly handled by the runtime (eaten as stale, not causing any issues)
    let history = store.read("select-loser-1").await.unwrap();

    // There should be exactly 1 TimerCreated (the loser timer from select2)
    let timer_created_count = history
        .iter()
        .filter(|e| matches!(&e.kind, duroxide::EventKind::TimerCreated { .. }))
        .count();
    assert_eq!(timer_created_count, 1, "expected 1 loser timer scheduled");

    // The loser timer's TimerFired event should be present (timer fired after orchestration completed)
    // but since the orchestration already completed, it's a stale event that gets ignored
    let timer_fired_count = history
        .iter()
        .filter(|e| matches!(&e.kind, duroxide::EventKind::TimerFired { .. }))
        .count();
    // The timer fires after orchestration completes, so TimerFired may or may not be in history
    // depending on timing. What matters is: if it's there, the runtime handled it gracefully.
    // Since the orchestration completed successfully, any stale event was properly ignored.
    assert!(
        timer_fired_count <= 1,
        "expected at most 1 timer fired event, got {timer_fired_count}"
    );

    // Verify orchestration completed (not failed due to stale event)
    let completed = history
        .iter()
        .any(|e| matches!(&e.kind, duroxide::EventKind::OrchestrationCompleted { .. }));
    assert!(completed, "orchestration should have completed successfully");

    rt.shutdown(None).await;
}

/// Regression test: simpler variant with explicit schedule after select2
#[tokio::test]
async fn test_select2_schedule_after_winner_returns() {
    let (store, _td) = common::create_sqlite_store_disk().await;

    let activities = ActivityRegistry::builder()
        .register("Instant", |_ctx: ActivityContext, _input: String| async move {
            // Returns instantly
            Ok("done".to_string())
        })
        .build();

    let orchestrations = OrchestrationRegistry::builder()
        .register("MinimalOrch", |ctx: OrchestrationContext, _input: String| async move {
            ctx.initialize_v2();
            // Race: instant activity vs 1 second timer
            // Activity wins immediately, timer is abandoned
            let mut timer = std::pin::pin!(ctx.schedule_timer_v2(Duration::from_secs(1)));
            let mut activity = std::pin::pin!(ctx.schedule_activity_v2("Instant", ""));
            
            let winner = futures::select! {
                result = activity => result?,
                _ = timer => return Err("timer won unexpectedly".to_string()),
            };

            // Now schedule another activity
            // Previously this would fail because the timer's scheduling event
            // wasn't consumed during replay
            let result = ctx.schedule_activity_v2("Instant", "").await?;

            Ok(result)
        })
        .build();

    let rt = runtime::Runtime::start_with_store(store.clone(), StdArc::new(activities), orchestrations).await;
    let client = duroxide::Client::new(store.clone());

    client
        .start_orchestration("minimal-1", "MinimalOrch", "")
        .await
        .unwrap();

    match client
        .wait_for_orchestration("minimal-1", Duration::from_secs(5))
        .await
        .unwrap()
    {
        OrchestrationStatus::Completed { output } => {
            assert_eq!(output, "done");
        }
        OrchestrationStatus::Failed { details } => {
            let msg = details.display_message();
            panic!("should not fail with nondeterminism anymore: {msg}");
        }
        other => panic!("unexpected status: {other:?}"),
    }

    rt.shutdown(None).await;
}
