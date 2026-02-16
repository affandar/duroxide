//! Session support validation tests.
//!
//! Validates that providers correctly implement session-based worker affinity:
//! - Session creation, claiming, and releasing
//! - Session work item routing (prioritizes session-owned work)
//! - Lock renewal and expiration
//! - Regular (non-session) work items still flow correctly

use super::*;
use crate::providers::{ExecutionMetadata, Provider, ScheduledSessionIdentifier, WorkItem};
use std::sync::Arc;

const TEST_SESSION_LOCK: Duration = Duration::from_secs(60);

/// Helper to create a session-bound activity work item.
fn session_activity(instance: &str, session_id: &str, activity_id: u64) -> WorkItem {
    WorkItem::ActivityExecute {
        instance: instance.to_string(),
        execution_id: INITIAL_EXECUTION_ID,
        id: activity_id,
        name: "SessionTask".to_string(),
        input: "{}".to_string(),
        session_id: Some(session_id.to_string()),
    }
}

/// Helper to create a regular (non-session) activity work item.
fn regular_activity(instance: &str, activity_id: u64) -> WorkItem {
    WorkItem::ActivityExecute {
        instance: instance.to_string(),
        execution_id: INITIAL_EXECUTION_ID,
        id: activity_id,
        name: "RegularTask".to_string(),
        input: "{}".to_string(),
        session_id: None,
    }
}

/// Test: Provider reports session support correctly.
pub async fn test_supports_sessions(provider: Arc<dyn Provider>) {
    // SQLite provider should support sessions
    assert!(provider.supports_sessions(), "provider should report session support");
}

/// Test: Session-bound work items are routed to the session-owning worker.
///
/// Scenario:
/// 1. Worker A claims a session by fetching a session-bound item
/// 2. Worker B enqueues another item for the same session
/// 3. Worker A should get it; Worker B should not
pub async fn test_session_affinity_routing(factory: &dyn super::ProviderFactory) {
    let provider = factory.create_provider().await;
    let instance = "session-route-test";
    let session_id = "ses-1";

    // Create the instance
    create_instance(&*provider, instance).await.unwrap();

    // Enqueue first session-bound item
    provider
        .enqueue_for_worker(session_activity(instance, session_id, 1))
        .await
        .unwrap();

    // Worker A claims it (which also claims the session)
    let result = provider
        .fetch_session_work_item(Duration::from_secs(30), Duration::ZERO, "worker-A", TEST_SESSION_LOCK)
        .await
        .unwrap();

    assert!(result.is_some(), "worker-A should get session item");
    let (item, token, _count) = result.unwrap();
    match &item {
        WorkItem::ActivityExecute {
            id, session_id: sid, ..
        } => {
            assert_eq!(*id, 1);
            assert_eq!(sid.as_deref(), Some("ses-1"));
        }
        _ => panic!("expected ActivityExecute"),
    }
    // Ack it
    provider.ack_work_item(&token, None).await.unwrap();

    // Enqueue second item for same session
    provider
        .enqueue_for_worker(session_activity(instance, session_id, 2))
        .await
        .unwrap();

    // Worker A should get it because session is owned by A
    let result_a = provider
        .fetch_session_work_item(Duration::from_secs(30), Duration::ZERO, "worker-A", TEST_SESSION_LOCK)
        .await
        .unwrap();
    assert!(result_a.is_some(), "worker-A should get second session item");
    let (_item, token_a, _) = result_a.unwrap();
    provider.ack_work_item(&token_a, None).await.unwrap();
}

/// Test: Regular (non-session) work items still flow to any worker.
pub async fn test_regular_items_unaffected(factory: &dyn super::ProviderFactory) {
    let provider = factory.create_provider().await;
    let instance = "session-regular-test";
    create_instance(&*provider, instance).await.unwrap();

    // Enqueue a regular item
    provider
        .enqueue_for_worker(regular_activity(instance, 1))
        .await
        .unwrap();

    // Any worker should get it via fetch_session_work_item
    let result = provider
        .fetch_session_work_item(Duration::from_secs(30), Duration::ZERO, "worker-X", TEST_SESSION_LOCK)
        .await
        .unwrap();
    assert!(result.is_some(), "regular item should be fetchable");
    let (item, token, _) = result.unwrap();
    match &item {
        WorkItem::ActivityExecute { session_id, .. } => {
            assert!(session_id.is_none(), "regular item should have no session_id");
        }
        _ => panic!("expected ActivityExecute"),
    }
    provider.ack_work_item(&token, None).await.unwrap();
}

/// Test: Session lock renewal works.
pub async fn test_session_lock_renewal(factory: &dyn super::ProviderFactory) {
    let provider = factory.create_provider().await;
    let instance = "session-renewal-test";
    let session_id = "ses-renew";

    create_instance(&*provider, instance).await.unwrap();

    // Enqueue and claim a session item
    provider
        .enqueue_for_worker(session_activity(instance, session_id, 1))
        .await
        .unwrap();

    let result = provider
        .fetch_session_work_item(Duration::from_secs(5), Duration::ZERO, "worker-R", TEST_SESSION_LOCK)
        .await
        .unwrap();
    assert!(result.is_some());
    let (_item, token, _) = result.unwrap();
    provider.ack_work_item(&token, None).await.unwrap();

    // Renew session lock
    let renew_result = provider
        .renew_session_lock(instance, session_id, "worker-R", Duration::from_secs(60))
        .await;
    assert!(renew_result.is_ok(), "session lock renewal should work");

    // Renewal by wrong worker should fail
    let wrong_renew = provider
        .renew_session_lock(instance, session_id, "worker-WRONG", Duration::from_secs(60))
        .await;
    assert!(wrong_renew.is_err(), "renewal by wrong worker should fail");
}

/// Test: Session lock release makes session available for others.
pub async fn test_session_release(factory: &dyn super::ProviderFactory) {
    let provider = factory.create_provider().await;
    let instance = "session-release-test";
    let session_id = "ses-release";

    create_instance(&*provider, instance).await.unwrap();

    // Claim session
    provider
        .enqueue_for_worker(session_activity(instance, session_id, 1))
        .await
        .unwrap();
    let result = provider
        .fetch_session_work_item(Duration::from_secs(30), Duration::ZERO, "worker-1", TEST_SESSION_LOCK)
        .await
        .unwrap();
    let (_item, token, _) = result.unwrap();
    provider.ack_work_item(&token, None).await.unwrap();

    // Release session
    provider
        .release_session_lock(instance, session_id, "worker-1")
        .await
        .unwrap();

    // Now another worker should be able to claim new work for that session
    provider
        .enqueue_for_worker(session_activity(instance, session_id, 2))
        .await
        .unwrap();

    let result2 = provider
        .fetch_session_work_item(Duration::from_secs(30), Duration::ZERO, "worker-2", TEST_SESSION_LOCK)
        .await
        .unwrap();
    assert!(result2.is_some(), "worker-2 should claim released session");
    let (_item, token2, _) = result2.unwrap();
    provider.ack_work_item(&token2, None).await.unwrap();
}

/// Test: Mixed session and regular items both flow correctly.
pub async fn test_mixed_session_and_regular(factory: &dyn super::ProviderFactory) {
    let provider = factory.create_provider().await;
    let instance = "session-mixed-test";
    let session_id = "ses-mixed";

    create_instance(&*provider, instance).await.unwrap();

    // Enqueue: one session item, one regular
    provider
        .enqueue_for_worker(session_activity(instance, session_id, 1))
        .await
        .unwrap();
    provider
        .enqueue_for_worker(regular_activity(instance, 2))
        .await
        .unwrap();

    // Worker should be able to get both
    let mut fetched_count = 0;
    for _ in 0..3 {
        let result = provider
            .fetch_session_work_item(Duration::from_secs(5), Duration::ZERO, "worker-M", TEST_SESSION_LOCK)
            .await
            .unwrap();
        if let Some((_item, token, _)) = result {
            provider.ack_work_item(&token, None).await.unwrap();
            fetched_count += 1;
        }
    }
    assert_eq!(fetched_count, 2, "should fetch both session and regular items");
}

/// Test: FIFO ordering is respected — session-bound items do NOT starve regular items.
///
/// Scenario:
/// 1. Enqueue: regular(id=1), session-bound(id=2), regular(id=3)
/// 2. Worker fetches should return items in FIFO order (id=1, id=2, id=3)
///    NOT session-first (which would be id=2 before id=1).
pub async fn test_session_fifo_no_starvation(factory: &dyn super::ProviderFactory) {
    let provider = factory.create_provider().await;
    let instance = "session-fifo-test";
    let session_id = "ses-fifo";

    create_instance(&*provider, instance).await.unwrap();

    // Enqueue in order: regular, session, regular
    provider
        .enqueue_for_worker(regular_activity(instance, 1))
        .await
        .unwrap();
    provider
        .enqueue_for_worker(session_activity(instance, session_id, 2))
        .await
        .unwrap();
    provider
        .enqueue_for_worker(regular_activity(instance, 3))
        .await
        .unwrap();

    // Fetch should respect FIFO: id=1 first (regular), then id=2 (session), then id=3 (regular)
    let mut fetched_ids = Vec::new();
    for _ in 0..4 {
        let result = provider
            .fetch_session_work_item(Duration::from_secs(5), Duration::ZERO, "worker-F", TEST_SESSION_LOCK)
            .await
            .unwrap();
        if let Some((item, token, _)) = result {
            match &item {
                WorkItem::ActivityExecute { id, .. } => fetched_ids.push(*id),
                _ => panic!("expected ActivityExecute"),
            }
            provider.ack_work_item(&token, None).await.unwrap();
        }
    }
    assert_eq!(
        fetched_ids,
        vec![1, 2, 3],
        "items should be fetched in FIFO order, not session-first"
    );
}

/// Test: Fetching an owned-session item extends the session lock.
///
/// Scenario:
/// 1. Worker A claims a session with short lock (2s)
/// 2. Wait 1s, then enqueue another item for the same session
/// 3. Worker A fetches the second item — this should extend the session lock
/// 4. Worker B should NOT be able to claim the session (lock is fresh)
pub async fn test_session_lock_extended_on_fetch(factory: &dyn super::ProviderFactory) {
    let provider = factory.create_provider().await;
    let instance = "session-lock-extend-test";
    let session_id = "ses-extend";

    create_instance(&*provider, instance).await.unwrap();

    // Worker A claims the session with a short lock
    provider
        .enqueue_for_worker(session_activity(instance, session_id, 1))
        .await
        .unwrap();

    let result = provider
        .fetch_session_work_item(Duration::from_secs(2), Duration::ZERO, "worker-A", TEST_SESSION_LOCK)
        .await
        .unwrap();
    assert!(result.is_some());
    let (_item, token, _) = result.unwrap();
    provider.ack_work_item(&token, None).await.unwrap();

    // Enqueue second item for same session
    provider
        .enqueue_for_worker(session_activity(instance, session_id, 2))
        .await
        .unwrap();

    // Worker A fetches it — this should extend the session lock
    let result2 = provider
        .fetch_session_work_item(Duration::from_secs(30), Duration::ZERO, "worker-A", TEST_SESSION_LOCK)
        .await
        .unwrap();
    assert!(result2.is_some(), "worker-A should get second session item");
    let (_item2, token2, _) = result2.unwrap();
    provider.ack_work_item(&token2, None).await.unwrap();

    // Enqueue third item — Worker B should NOT get it (session lock was just extended)
    provider
        .enqueue_for_worker(session_activity(instance, session_id, 3))
        .await
        .unwrap();
    let result_b = provider
        .fetch_session_work_item(Duration::from_secs(5), Duration::ZERO, "worker-B", TEST_SESSION_LOCK)
        .await
        .unwrap();
    // Worker B should NOT get this session item because A still owns the session
    if let Some((item, _, _)) = &result_b {
        match item {
            WorkItem::ActivityExecute { session_id: sid, .. } => {
                assert!(
                    sid.is_none(),
                    "worker-B should only get a non-session item, not a session-bound item owned by A"
                );
            }
            _ => {}
        }
    }
}

/// Test: Session-bound items owned by another worker are skipped, not returned.
///
/// Scenario:
/// 1. Worker A claims session "ses-block"
/// 2. Enqueue: session-bound item (owned by A), then regular item
/// 3. Worker B should skip the session item (owned by A) and get the regular item
pub async fn test_session_item_skipped_when_owned_by_other(factory: &dyn super::ProviderFactory) {
    let provider = factory.create_provider().await;
    let instance = "session-skip-test";
    let session_id = "ses-block";

    create_instance(&*provider, instance).await.unwrap();

    // Worker A claims the session
    provider
        .enqueue_for_worker(session_activity(instance, session_id, 1))
        .await
        .unwrap();
    let result = provider
        .fetch_session_work_item(Duration::from_secs(60), Duration::ZERO, "worker-A", TEST_SESSION_LOCK)
        .await
        .unwrap();
    assert!(result.is_some());
    let (_item, token, _) = result.unwrap();
    provider.ack_work_item(&token, None).await.unwrap();

    // Enqueue: session item (owned by A), then regular item
    provider
        .enqueue_for_worker(session_activity(instance, session_id, 2))
        .await
        .unwrap();
    provider
        .enqueue_for_worker(regular_activity(instance, 3))
        .await
        .unwrap();

    // Worker B should skip the session item and get the regular one
    let result_b = provider
        .fetch_session_work_item(Duration::from_secs(5), Duration::ZERO, "worker-B", TEST_SESSION_LOCK)
        .await
        .unwrap();
    assert!(result_b.is_some(), "worker-B should get the regular item");
    let (item_b, token_b, _) = result_b.unwrap();
    match &item_b {
        WorkItem::ActivityExecute {
            id, session_id: sid, ..
        } => {
            assert_eq!(
                *id, 3,
                "worker-B should get the regular item (id=3), not the session item (id=2)"
            );
            assert!(sid.is_none(), "should be a regular (non-session) item");
        }
        _ => panic!("expected ActivityExecute"),
    }
    provider.ack_work_item(&token_b, None).await.unwrap();
}

/// Test: Plain fetch (`fetch_work_item`) only returns non-session items.
///
/// Scenario:
/// 1. Enqueue session-bound item first, then regular item
/// 2. `fetch_work_item` should skip the session item and return the regular one
pub async fn test_plain_fetch_only_non_session(factory: &dyn super::ProviderFactory) {
    let provider = factory.create_provider().await;
    let instance = "plain-fetch-non-session-only";
    let session_id = "ses-plain";

    create_instance(&*provider, instance).await.unwrap();

    provider
        .enqueue_for_worker(session_activity(instance, session_id, 1))
        .await
        .unwrap();
    provider
        .enqueue_for_worker(regular_activity(instance, 2))
        .await
        .unwrap();

    let result = provider
        .fetch_work_item(Duration::from_secs(30), Duration::ZERO)
        .await
        .unwrap();
    assert!(result.is_some(), "plain fetch should find regular item");

    let (item, token, _) = result.unwrap();
    match item {
        WorkItem::ActivityExecute {
            id, session_id: sid, ..
        } => {
            assert_eq!(id, 2, "plain fetch should return regular item id=2");
            assert!(sid.is_none(), "plain fetch must not return session-bound items");
        }
        _ => panic!("expected ActivityExecute"),
    }
    provider.ack_work_item(&token, None).await.unwrap();
}

/// Test: Session-aware fetch (`fetch_session_work_item`) can fetch both regular and session items.
///
/// Scenario:
/// 1. Enqueue one regular and one session-bound item
/// 2. Session-aware worker should be able to fetch both over successive calls
pub async fn test_session_fetch_mixed_behavior(factory: &dyn super::ProviderFactory) {
    let provider = factory.create_provider().await;
    let instance = "session-fetch-mixed";
    let session_id = "ses-mixed-behavior";

    create_instance(&*provider, instance).await.unwrap();

    provider
        .enqueue_for_worker(regular_activity(instance, 1))
        .await
        .unwrap();
    provider
        .enqueue_for_worker(session_activity(instance, session_id, 2))
        .await
        .unwrap();

    let mut saw_regular = false;
    let mut saw_session = false;

    for _ in 0..3 {
        let result = provider
            .fetch_session_work_item(Duration::from_secs(30), Duration::ZERO, "worker-MIX", TEST_SESSION_LOCK)
            .await
            .unwrap();

        if let Some((item, token, _)) = result {
            match item {
                WorkItem::ActivityExecute { session_id: sid, .. } => {
                    if sid.is_some() {
                        saw_session = true;
                    } else {
                        saw_regular = true;
                    }
                }
                _ => panic!("expected ActivityExecute"),
            }
            provider.ack_work_item(&token, None).await.unwrap();
        }
    }

    assert!(saw_regular, "session-aware fetch should return regular items too");
    assert!(saw_session, "session-aware fetch should return session-bound items");
}

/// Test: concurrent session claim race has a single winner.
///
/// Scenario:
/// 1. Enqueue one session-bound item for an unclaimed session
/// 2. Two workers race to fetch at the same time
/// 3. Exactly one worker should receive the item
pub async fn test_session_claim_race_single_winner(factory: &dyn super::ProviderFactory) {
    async fn fetch_session_with_retry(
        provider: Arc<dyn Provider>,
        worker_id: &'static str,
    ) -> Option<(WorkItem, String, u32)> {
        let mut attempts = 0u32;
        loop {
            match provider
                .fetch_session_work_item(Duration::from_secs(30), Duration::ZERO, worker_id, TEST_SESSION_LOCK)
                .await
            {
                Ok(v) => return v,
                Err(e) if e.is_retryable() && attempts < 10 => {
                    attempts += 1;
                    tokio::time::sleep(Duration::from_millis(10 * attempts as u64)).await;
                }
                Err(e) => panic!("fetch_session_work_item failed during race test: {e:?}"),
            }
        }
    }

    let provider = factory.create_provider().await;
    let instance = "session-claim-race";
    create_instance(&*provider, instance).await.unwrap();

    for idx in 0..50u64 {
        let sid = format!("race-session-{idx}");
        provider
            .enqueue_for_worker(session_activity(instance, &sid, idx + 1))
            .await
            .unwrap();

        let p1 = Arc::clone(&provider);
        let p2 = Arc::clone(&provider);

        let (r1, r2) = tokio::join!(
            fetch_session_with_retry(p1, "worker-race-A"),
            fetch_session_with_retry(p2, "worker-race-B")
        );

        let winner_count = usize::from(r1.is_some()) + usize::from(r2.is_some());
        assert_eq!(
            winner_count, 1,
            "exactly one worker should win race for session {sid}, got {winner_count}"
        );

        if let Some((_item, token, _)) = r1 {
            provider.ack_work_item(&token, None).await.unwrap();
        }
        if let Some((_item, token, _)) = r2 {
            provider.ack_work_item(&token, None).await.unwrap();
        }
    }
}

/// Test: close-session side-channel performs lock stealing.
///
/// This validates provider behavior for runtime-driven close_session handling:
/// - session-bound worker rows are deleted (activity lock stolen)
/// - session ownership row is deleted (next session renewal fails)
pub async fn test_close_session_lock_stealing_signal(factory: &dyn super::ProviderFactory) {
    let provider = factory.create_provider().await;
    let instance = "session-close-lock-steal";
    let session_id = "ses-close-steal";

    create_instance(&*provider, instance).await.unwrap();

    provider
        .enqueue_for_worker(session_activity(instance, session_id, 1))
        .await
        .unwrap();

    let fetched = provider
        .fetch_session_work_item(Duration::from_secs(30), Duration::ZERO, "worker-close-A", TEST_SESSION_LOCK)
        .await
        .unwrap()
        .expect("worker should fetch session item");
    let (_item, worker_lock_token, _) = fetched;

    // Acquire an orchestration lock token so we can drive atomic ack side-channel behavior.
    provider
        .enqueue_for_orchestrator(
            WorkItem::ExternalRaised {
                instance: instance.to_string(),
                name: "close-session-test".to_string(),
                data: "{}".to_string(),
            },
            None,
        )
        .await
        .unwrap();

    let (orch_item, orch_lock_token, _) = provider
        .fetch_orchestration_item(Duration::from_secs(30), Duration::ZERO, None)
        .await
        .unwrap()
        .expect("orchestration item should be fetchable");

    provider
        .ack_orchestration_item(
            &orch_lock_token,
            orch_item.execution_id,
            vec![],
            vec![],
            vec![],
            ExecutionMetadata {
                cancelled_sessions: vec![ScheduledSessionIdentifier {
                    instance: instance.to_string(),
                    session_id: session_id.to_string(),
                }],
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();

    // Worker-row lock should be stolen (row deleted), so renew_work_item_lock fails.
    let renew_work = provider
        .renew_work_item_lock(&worker_lock_token, Duration::from_secs(5))
        .await;
    assert!(
        renew_work.is_err(),
        "work item lock renewal should fail after lock stealing"
    );

    // Session row should be deleted, so renew_session_lock fails.
    let renew_session = provider
        .renew_session_lock(instance, session_id, "worker-close-A", Duration::from_secs(30))
        .await;
    assert!(
        renew_session.is_err(),
        "session renewal should fail after session close lock stealing"
    );
}
