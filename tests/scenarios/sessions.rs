//! Activity-explicit sessions scenario tests
//!
//! End-to-end tests validating the session lifecycle:
//! - Open a session, schedule activities on it, close it
//! - Verify `session_id` flows through ActivityContext
//! - Verify session events appear in history
//! - Verify replay determinism with sessions

use duroxide::runtime::registry::ActivityRegistry;
use duroxide::runtime::{self, RuntimeOptions};
use duroxide::{ActivityContext, Client, OrchestrationContext, OrchestrationRegistry, OrchestrationStatus};
use std::time::Duration;

#[path = "../common/mod.rs"]
mod common;

/// Basic session lifecycle: open → schedule_activity_on_session → close
#[tokio::test]
async fn session_basic_lifecycle() {
    let (store, _td) = common::create_sqlite_store_disk().await;

    let activity_registry = ActivityRegistry::builder()
        .register("SessionTask", |ctx: ActivityContext, input: String| async move {
            let sid = ctx.session_id().unwrap_or("none").to_string();
            Ok(format!("session={sid}, input={input}"))
        })
        .build();

    let orchestration = |ctx: OrchestrationContext, _input: String| async move {
        // Open a session
        let sid = ctx.open_session_with_id("my-session")?;
        assert_eq!(sid, "my-session");

        // Schedule activity on session
        let result = ctx.schedule_activity_on_session("SessionTask", "hello", &sid).await?;

        // Close session
        ctx.close_session(&sid);

        Ok(result)
    };

    let orchestrations = OrchestrationRegistry::builder()
        .register("SessionOrch", orchestration)
        .build();

    let rt = runtime::Runtime::start_with_store(store.clone(), activity_registry, orchestrations).await;

    let client = Client::new(store.clone());
    client
        .start_orchestration("session-lifecycle", "SessionOrch", "{}")
        .await
        .unwrap();

    let status = client
        .wait_for_orchestration("session-lifecycle", Duration::from_secs(10))
        .await
        .unwrap();

    match status {
        OrchestrationStatus::Completed { output } => {
            assert!(
                output.contains("session=my-session"),
                "activity should see session_id, got: {output}"
            );
        }
        other => panic!("expected Completed, got: {other:?}"),
    }

    // Verify session events in history
    let history = store.read("session-lifecycle").await.unwrap();
    let session_opened = history
        .iter()
        .any(|e| matches!(&e.kind, duroxide::EventKind::SessionOpened { session_id } if session_id == "my-session"));
    let session_closed = history
        .iter()
        .any(|e| matches!(&e.kind, duroxide::EventKind::SessionClosed { session_id } if session_id == "my-session"));
    let activity_with_session = history.iter().any(|e| {
        matches!(
            &e.kind,
            duroxide::EventKind::ActivityScheduled { session_id: Some(sid), .. } if sid == "my-session"
        )
    });

    assert!(session_opened, "history should contain SessionOpened");
    assert!(session_closed, "history should contain SessionClosed");
    assert!(
        activity_with_session,
        "history should contain ActivityScheduled with session_id"
    );

    rt.shutdown(None).await;
}

/// Regular activities (no session) still work alongside session activities.
#[tokio::test]
async fn session_mixed_with_regular_activities() {
    let (store, _td) = common::create_sqlite_store_disk().await;

    let activity_registry = ActivityRegistry::builder()
        .register("SessionTask", |_ctx: ActivityContext, input: String| async move {
            Ok(format!("session:{input}"))
        })
        .register("RegularTask", |_ctx: ActivityContext, input: String| async move {
            Ok(format!("regular:{input}"))
        })
        .build();

    let orchestration = |ctx: OrchestrationContext, _input: String| async move {
        // Regular activity (no session)
        let r1 = ctx.schedule_activity("RegularTask", "a").await?;

        // Session activity
        let sid = ctx.open_session_with_id("ses-mix")?;
        let r2 = ctx.schedule_activity_on_session("SessionTask", "b", &sid).await?;
        ctx.close_session(&sid);

        Ok(format!("{r1},{r2}"))
    };

    let orchestrations = OrchestrationRegistry::builder()
        .register("MixedOrch", orchestration)
        .build();

    let rt = runtime::Runtime::start_with_store(store.clone(), activity_registry, orchestrations).await;

    let client = Client::new(store.clone());
    client
        .start_orchestration("mixed-test", "MixedOrch", "{}")
        .await
        .unwrap();

    let status = client
        .wait_for_orchestration("mixed-test", Duration::from_secs(10))
        .await
        .unwrap();

    match status {
        OrchestrationStatus::Completed { output } => {
            assert_eq!(output, "regular:a,session:b");
        }
        other => panic!("expected Completed, got: {other:?}"),
    }

    rt.shutdown(None).await;
}

/// Auto-generated session ID (via ctx.open_session())
#[tokio::test]
async fn session_auto_generated_id() {
    let (store, _td) = common::create_sqlite_store_disk().await;

    let activity_registry = ActivityRegistry::builder()
        .register("STask", |ctx: ActivityContext, _input: String| async move {
            let sid = ctx.session_id().unwrap_or("none").to_string();
            Ok(sid)
        })
        .build();

    let orchestration = |ctx: OrchestrationContext, _input: String| async move {
        let sid = ctx.open_session()?;
        // Session ID should be deterministic and non-empty
        assert!(!sid.is_empty(), "auto-generated session ID should not be empty");

        let result = ctx.schedule_activity_on_session("STask", "x", &sid).await?;
        ctx.close_session(&sid);
        Ok(result)
    };

    let orchestrations = OrchestrationRegistry::builder()
        .register("AutoSessionOrch", orchestration)
        .build();

    let rt = runtime::Runtime::start_with_store(store.clone(), activity_registry, orchestrations).await;

    let client = Client::new(store.clone());
    client
        .start_orchestration("auto-session", "AutoSessionOrch", "{}")
        .await
        .unwrap();

    let status = client
        .wait_for_orchestration("auto-session", Duration::from_secs(10))
        .await
        .unwrap();

    match status {
        OrchestrationStatus::Completed { output } => {
            assert!(
                output.starts_with("session-"),
                "auto-generated session ID should start with 'session-', got: {output}"
            );
        }
        other => panic!("expected Completed, got: {other:?}"),
    }

    rt.shutdown(None).await;
}

/// Session in single-threaded mode (1x1 concurrency).
#[tokio::test(flavor = "current_thread")]
async fn session_single_thread_mode() {
    let (store, _td) = common::create_sqlite_store_disk().await;

    let activity_registry = ActivityRegistry::builder()
        .register("STTask", |_ctx: ActivityContext, input: String| async move {
            Ok(format!("done:{input}"))
        })
        .build();

    let orchestration = |ctx: OrchestrationContext, _input: String| async move {
        let sid = ctx.open_session_with_id("st-session")?;
        let r = ctx.schedule_activity_on_session("STTask", "1", &sid).await?;
        ctx.close_session(&sid);
        Ok(r)
    };

    let orchestrations = OrchestrationRegistry::builder()
        .register("STSessionOrch", orchestration)
        .build();

    let rt = runtime::Runtime::start_with_options(
        store.clone(),
        activity_registry,
        orchestrations,
        RuntimeOptions {
            orchestration_concurrency: 1,
            worker_concurrency: 1,
            ..Default::default()
        },
    )
    .await;

    let client = Client::new(store.clone());
    client
        .start_orchestration("st-session-test", "STSessionOrch", "{}")
        .await
        .unwrap();

    let status = client
        .wait_for_orchestration("st-session-test", Duration::from_secs(10))
        .await
        .unwrap();

    assert!(matches!(status, OrchestrationStatus::Completed { .. }));

    rt.shutdown(None).await;
}

/// Exceeding max_sessions_per_orchestration fails the orchestration.
#[tokio::test]
async fn session_max_sessions_exceeded() {
    let (store, _td) = common::create_sqlite_store_disk().await;

    let activity_registry = ActivityRegistry::builder()
        .register("Noop", |_ctx: ActivityContext, _input: String| async move {
            Ok("ok".to_string())
        })
        .build();

    // Orchestration opens more sessions than allowed (limit=2)
    let orchestration = |ctx: OrchestrationContext, _input: String| async move {
        let _s1 = ctx.open_session_with_id("s1")?;
        let _s2 = ctx.open_session_with_id("s2")?;
        // This should fail because max_sessions_per_orchestration = 2
        let _s3 = ctx.open_session_with_id("s3")?;

        Ok("should not reach".to_string())
    };

    let orchestrations = OrchestrationRegistry::builder()
        .register("MaxSessionOrch", orchestration)
        .build();

    let rt = runtime::Runtime::start_with_options(
        store.clone(),
        activity_registry,
        orchestrations,
        RuntimeOptions {
            max_sessions_per_orchestration: 2,
            ..Default::default()
        },
    )
    .await;

    let client = Client::new(store.clone());
    client
        .start_orchestration("max-session", "MaxSessionOrch", "{}")
        .await
        .unwrap();

    let status = client
        .wait_for_orchestration("max-session", Duration::from_secs(10))
        .await
        .unwrap();

    match status {
        OrchestrationStatus::Failed { details } => {
            let msg = format!("{details:?}");
            assert!(
                msg.contains("max sessions per orchestration exceeded"),
                "expected max sessions error, got: {msg}"
            );
        }
        other => panic!("expected Failed, got: {other:?}"),
    }

    rt.shutdown(None).await;
}

/// Scheduling an activity on a closed session fails the orchestration.
#[tokio::test]
async fn session_activity_on_closed_session_fails() {
    let (store, _td) = common::create_sqlite_store_disk().await;

    let activity_registry = ActivityRegistry::builder()
        .register("Noop", |_ctx: ActivityContext, _input: String| async move {
            Ok("ok".to_string())
        })
        .build();

    let orchestration = |ctx: OrchestrationContext, _input: String| async move {
        let sid = ctx.open_session_with_id("ephemeral")?;
        ctx.close_session(&sid);
        // This should fail — session is closed
        let _r = ctx.schedule_activity_on_session("Noop", "x", &sid).await?;
        Ok("should not reach".to_string())
    };

    let orchestrations = OrchestrationRegistry::builder()
        .register("ClosedSessionOrch", orchestration)
        .build();

    let rt = runtime::Runtime::start_with_store(store.clone(), activity_registry, orchestrations).await;

    let client = Client::new(store.clone());
    client
        .start_orchestration("closed-session", "ClosedSessionOrch", "{}")
        .await
        .unwrap();

    let status = client
        .wait_for_orchestration("closed-session", Duration::from_secs(10))
        .await
        .unwrap();

    match status {
        OrchestrationStatus::Failed { details } => {
            let msg = format!("{details:?}");
            assert!(msg.contains("not open"), "expected 'not open' error, got: {msg}");
        }
        other => panic!("expected Failed, got: {other:?}"),
    }

    rt.shutdown(None).await;
}
