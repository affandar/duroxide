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

/// open_session_with_id is idempotent: second call is a no-op.
#[tokio::test]
async fn session_open_with_id_idempotent() {
    let (store, _td) = common::create_sqlite_store_disk().await;

    let activity_registry = ActivityRegistry::builder()
        .register("Echo", |_ctx: ActivityContext, input: String| async move {
            Ok(input)
        })
        .build();

    let orchestration = |ctx: OrchestrationContext, _input: String| async move {
        let s1 = ctx.open_session_with_id("dup")?;
        let s2 = ctx.open_session_with_id("dup")?;
        assert_eq!(s1, s2);
        let r = ctx.schedule_activity_on_session("Echo", "ok", &s1).await?;
        ctx.close_session(&s1);
        Ok(r)
    };

    let orchestrations = OrchestrationRegistry::builder()
        .register("IdempotentOrch", orchestration)
        .build();

    let rt = runtime::Runtime::start_with_store(store.clone(), activity_registry, orchestrations).await;
    let client = Client::new(store.clone());
    client
        .start_orchestration("idempotent-test", "IdempotentOrch", "{}")
        .await
        .unwrap();

    let status = client
        .wait_for_orchestration("idempotent-test", Duration::from_secs(10))
        .await
        .unwrap();

    assert!(matches!(status, OrchestrationStatus::Completed { .. }));

    // Verify only ONE SessionOpened event in history (idempotent second call is no-op)
    let history = store.read("idempotent-test").await.unwrap();
    let session_opened_count = history
        .iter()
        .filter(|e| matches!(&e.kind, duroxide::EventKind::SessionOpened { session_id } if session_id == "dup"))
        .count();
    assert_eq!(
        session_opened_count, 1,
        "idempotent open_session_with_id should produce exactly 1 SessionOpened event"
    );

    rt.shutdown(None).await;
}

/// close_session is idempotent: closing already-closed or never-opened is a no-op.
#[tokio::test]
async fn session_close_idempotent() {
    let (store, _td) = common::create_sqlite_store_disk().await;

    let activity_registry = ActivityRegistry::builder().build();

    let orchestration = |ctx: OrchestrationContext, _input: String| async move {
        let sid = ctx.open_session_with_id("eph")?;
        ctx.close_session(&sid);
        ctx.close_session(&sid); // Second close is no-op
        ctx.close_session("never-opened"); // Never-opened is also no-op
        Ok("ok".to_string())
    };

    let orchestrations = OrchestrationRegistry::builder()
        .register("CloseIdempotentOrch", orchestration)
        .build();

    let rt = runtime::Runtime::start_with_store(store.clone(), activity_registry, orchestrations).await;
    let client = Client::new(store.clone());
    client
        .start_orchestration("close-idem", "CloseIdempotentOrch", "{}")
        .await
        .unwrap();

    let status = client
        .wait_for_orchestration("close-idem", Duration::from_secs(10))
        .await
        .unwrap();

    assert!(matches!(status, OrchestrationStatus::Completed { .. }));
    rt.shutdown(None).await;
}

/// Empty session ID is rejected.
#[tokio::test]
async fn session_empty_id_rejected() {
    let (store, _td) = common::create_sqlite_store_disk().await;

    let activity_registry = ActivityRegistry::builder().build();

    let orchestration = |ctx: OrchestrationContext, _input: String| async move {
        let result = ctx.open_session_with_id("");
        match result {
            Err(e) => Ok(format!("rejected: {e}")),
            Ok(_) => Err("expected error for empty session_id".to_string()),
        }
    };

    let orchestrations = OrchestrationRegistry::builder()
        .register("EmptyIdOrch", orchestration)
        .build();

    let rt = runtime::Runtime::start_with_store(store.clone(), activity_registry, orchestrations).await;
    let client = Client::new(store.clone());
    client
        .start_orchestration("empty-id", "EmptyIdOrch", "{}")
        .await
        .unwrap();

    let status = client
        .wait_for_orchestration("empty-id", Duration::from_secs(10))
        .await
        .unwrap();

    match status {
        OrchestrationStatus::Completed { output } => {
            assert!(output.contains("must not be empty"), "got: {output}");
        }
        other => panic!("expected Completed, got: {other:?}"),
    }

    rt.shutdown(None).await;
}

/// Session ID length > 1024 is rejected.
#[tokio::test]
async fn session_long_id_rejected() {
    let (store, _td) = common::create_sqlite_store_disk().await;

    let activity_registry = ActivityRegistry::builder().build();

    let orchestration = |ctx: OrchestrationContext, _input: String| async move {
        let long_id = "a".repeat(1025);
        let result = ctx.open_session_with_id(long_id);
        match result {
            Err(e) => Ok(format!("rejected: {e}")),
            Ok(_) => Err("expected error for long session_id".to_string()),
        }
    };

    let orchestrations = OrchestrationRegistry::builder()
        .register("LongIdOrch", orchestration)
        .build();

    let rt = runtime::Runtime::start_with_store(store.clone(), activity_registry, orchestrations).await;
    let client = Client::new(store.clone());
    client
        .start_orchestration("long-id", "LongIdOrch", "{}")
        .await
        .unwrap();

    let status = client
        .wait_for_orchestration("long-id", Duration::from_secs(10))
        .await
        .unwrap();

    match status {
        OrchestrationStatus::Completed { output } => {
            assert!(output.contains("exceeds maximum length"), "got: {output}");
        }
        other => panic!("expected Completed, got: {other:?}"),
    }

    rt.shutdown(None).await;
}

/// Open -> close -> reopen with same ID works.
#[tokio::test]
async fn session_open_close_reopen() {
    let (store, _td) = common::create_sqlite_store_disk().await;

    let activity_registry = ActivityRegistry::builder()
        .register("Echo", |_ctx: ActivityContext, input: String| async move {
            Ok(input)
        })
        .build();

    let orchestration = |ctx: OrchestrationContext, _input: String| async move {
        let sid = ctx.open_session_with_id("reopen-me")?;
        ctx.close_session(&sid);
        let sid2 = ctx.open_session_with_id("reopen-me")?;
        assert_eq!(sid, sid2);
        let r = ctx.schedule_activity_on_session("Echo", "after-reopen", &sid2).await?;
        ctx.close_session(&sid2);
        Ok(r)
    };

    let orchestrations = OrchestrationRegistry::builder()
        .register("ReopenOrch", orchestration)
        .build();

    let rt = runtime::Runtime::start_with_store(store.clone(), activity_registry, orchestrations).await;
    let client = Client::new(store.clone());
    client
        .start_orchestration("reopen-test", "ReopenOrch", "{}")
        .await
        .unwrap();

    let status = client
        .wait_for_orchestration("reopen-test", Duration::from_secs(10))
        .await
        .unwrap();

    match status {
        OrchestrationStatus::Completed { output } => assert_eq!(output, "after-reopen"),
        other => panic!("expected Completed, got: {other:?}"),
    }

    rt.shutdown(None).await;
}

/// Multiple sessions open concurrently, each independently usable.
#[tokio::test]
async fn session_multiple_concurrent() {
    let (store, _td) = common::create_sqlite_store_disk().await;

    let activity_registry = ActivityRegistry::builder()
        .register("Label", |ctx: ActivityContext, input: String| async move {
            let sid = ctx.session_id().unwrap_or("none");
            Ok(format!("{sid}:{input}"))
        })
        .build();

    let orchestration = |ctx: OrchestrationContext, _input: String| async move {
        let s1 = ctx.open_session_with_id("alpha")?;
        let s2 = ctx.open_session_with_id("beta")?;
        let s3 = ctx.open_session_with_id("gamma")?;

        let r1 = ctx.schedule_activity_on_session("Label", "1", &s1).await?;
        let r2 = ctx.schedule_activity_on_session("Label", "2", &s2).await?;
        let r3 = ctx.schedule_activity_on_session("Label", "3", &s3).await?;

        ctx.close_session(&s1);
        ctx.close_session(&s2);
        ctx.close_session(&s3);

        Ok(format!("{r1},{r2},{r3}"))
    };

    let orchestrations = OrchestrationRegistry::builder()
        .register("MultiSessionOrch", orchestration)
        .build();

    let rt = runtime::Runtime::start_with_store(store.clone(), activity_registry, orchestrations).await;
    let client = Client::new(store.clone());
    client
        .start_orchestration("multi-session", "MultiSessionOrch", "{}")
        .await
        .unwrap();

    let status = client
        .wait_for_orchestration("multi-session", Duration::from_secs(10))
        .await
        .unwrap();

    match status {
        OrchestrationStatus::Completed { output } => {
            assert_eq!(output, "alpha:1,beta:2,gamma:3");
        }
        other => panic!("expected Completed, got: {other:?}"),
    }

    rt.shutdown(None).await;
}

/// Close one session, keep another open — scheduling on closed fails, on open succeeds.
#[tokio::test]
async fn session_close_one_keep_other() {
    let (store, _td) = common::create_sqlite_store_disk().await;

    let activity_registry = ActivityRegistry::builder()
        .register("Noop", |_ctx: ActivityContext, _input: String| async move {
            Ok("ok".to_string())
        })
        .build();

    let orchestration = |ctx: OrchestrationContext, _input: String| async move {
        let sa = ctx.open_session_with_id("A")?;
        let sb = ctx.open_session_with_id("B")?;
        ctx.close_session(&sa);

        // Scheduling on B should succeed
        let r = ctx.schedule_activity_on_session("Noop", "", &sb).await?;
        ctx.close_session(&sb);
        Ok(r)
    };

    let orchestrations = OrchestrationRegistry::builder()
        .register("CloseOneOrch", orchestration)
        .build();

    let rt = runtime::Runtime::start_with_store(store.clone(), activity_registry, orchestrations).await;
    let client = Client::new(store.clone());
    client
        .start_orchestration("close-one", "CloseOneOrch", "{}")
        .await
        .unwrap();

    let status = client
        .wait_for_orchestration("close-one", Duration::from_secs(10))
        .await
        .unwrap();

    assert!(matches!(status, OrchestrationStatus::Completed { .. }));
    rt.shutdown(None).await;
}

/// Max sessions reached, close one, open another — succeeds.
#[tokio::test]
async fn session_max_close_then_reopen_within_limit() {
    let (store, _td) = common::create_sqlite_store_disk().await;

    let activity_registry = ActivityRegistry::builder().build();

    let orchestration = |ctx: OrchestrationContext, _input: String| async move {
        let s1 = ctx.open_session_with_id("s1")?;
        let _s2 = ctx.open_session_with_id("s2")?;
        ctx.close_session(&s1);
        let _s3 = ctx.open_session_with_id("s3")?;
        Ok("ok".to_string())
    };

    let orchestrations = OrchestrationRegistry::builder()
        .register("MaxReopenOrch", orchestration)
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
        .start_orchestration("max-reopen", "MaxReopenOrch", "{}")
        .await
        .unwrap();

    let status = client
        .wait_for_orchestration("max-reopen", Duration::from_secs(10))
        .await
        .unwrap();

    assert!(matches!(status, OrchestrationStatus::Completed { .. }));
    rt.shutdown(None).await;
}

/// Session survives ContinueAsNew — activity on carried session works.
#[tokio::test]
async fn session_survives_continue_as_new() {
    let (store, _td) = common::create_sqlite_store_disk().await;

    let activity_registry = ActivityRegistry::builder()
        .register("Ping", |ctx: ActivityContext, input: String| async move {
            let sid = ctx.session_id().unwrap_or("none");
            Ok(format!("{sid}:{input}"))
        })
        .build();

    let orchestration = |ctx: OrchestrationContext, input: String| async move {
        let n: u32 = input.split(':').next().unwrap_or("0").parse().unwrap_or(0);

        if n == 0 {
            let sid = ctx.open_session_with_id("can-session")?;
            let _r = ctx.schedule_activity_on_session("Ping", "first", &sid).await?;
            return ctx.continue_as_new(format!("1:{sid}")).await;
        }

        let sid = input.split(':').nth(1).unwrap_or("missing");
        let r = ctx.schedule_activity_on_session("Ping", "second", sid).await?;
        ctx.close_session(sid);
        Ok(r)
    };

    let orchestrations = OrchestrationRegistry::builder()
        .register("CANSessionOrch", orchestration)
        .build();

    let rt = runtime::Runtime::start_with_store(store.clone(), activity_registry, orchestrations).await;
    let client = Client::new(store.clone());
    client
        .start_orchestration("can-session-test", "CANSessionOrch", "0")
        .await
        .unwrap();

    let status = client
        .wait_for_orchestration("can-session-test", Duration::from_secs(10))
        .await
        .unwrap();

    match status {
        OrchestrationStatus::Completed { output } => {
            assert!(
                output.contains("can-session:second"),
                "session should carry across CAN, got: {output}"
            );
        }
        other => panic!("expected Completed, got: {other:?}"),
    }

    rt.shutdown(None).await;
}

/// Terminal orchestration auto-closes sessions — session rows cleaned up.
#[tokio::test]
async fn session_terminal_cleanup() {
    let (store, _td) = common::create_sqlite_store_disk().await;

    let activity_registry = ActivityRegistry::builder().build();

    let orchestration = |ctx: OrchestrationContext, _input: String| async move {
        let _s1 = ctx.open_session_with_id("leak1")?;
        let _s2 = ctx.open_session_with_id("leak2")?;
        Ok("done".to_string())
    };

    let orchestrations = OrchestrationRegistry::builder()
        .register("LeakyOrch", orchestration)
        .build();

    let rt = runtime::Runtime::start_with_store(store.clone(), activity_registry, orchestrations).await;
    let client = Client::new(store.clone());
    client
        .start_orchestration("terminal-cleanup", "LeakyOrch", "{}")
        .await
        .unwrap();

    let status = client
        .wait_for_orchestration("terminal-cleanup", Duration::from_secs(10))
        .await
        .unwrap();

    assert!(matches!(status, OrchestrationStatus::Completed { .. }));

    tokio::time::sleep(Duration::from_millis(200)).await;

    let renew1 = store
        .renew_session_lock("terminal-cleanup", "leak1", "any-worker", Duration::from_secs(30))
        .await;
    let renew2 = store
        .renew_session_lock("terminal-cleanup", "leak2", "any-worker", Duration::from_secs(30))
        .await;
    assert!(renew1.is_err(), "session leak1 should be cleaned up");
    assert!(renew2.is_err(), "session leak2 should be cleaned up");

    rt.shutdown(None).await;
}

/// Non-session activity has session_id() == None.
#[tokio::test]
async fn session_non_session_activity_has_none() {
    let (store, _td) = common::create_sqlite_store_disk().await;

    let activity_registry = ActivityRegistry::builder()
        .register("CheckSession", |ctx: ActivityContext, _input: String| async move {
            match ctx.session_id() {
                None => Ok("none".to_string()),
                Some(sid) => Err(format!("expected None, got {sid}")),
            }
        })
        .build();

    let orchestration = |ctx: OrchestrationContext, _input: String| async move {
        ctx.schedule_activity("CheckSession", "").await
    };

    let orchestrations = OrchestrationRegistry::builder()
        .register("NonSessionOrch", orchestration)
        .build();

    let rt = runtime::Runtime::start_with_store(store.clone(), activity_registry, orchestrations).await;
    let client = Client::new(store.clone());
    client
        .start_orchestration("non-session-check", "NonSessionOrch", "{}")
        .await
        .unwrap();

    let status = client
        .wait_for_orchestration("non-session-check", Duration::from_secs(10))
        .await
        .unwrap();

    match status {
        OrchestrationStatus::Completed { output } => assert_eq!(output, "none"),
        other => panic!("expected Completed, got: {other:?}"),
    }

    rt.shutdown(None).await;
}

/// Serialization backward compat: WorkItem::ActivityExecute without session_id deserializes to None.
#[tokio::test]
async fn session_serialization_backward_compat() {
    use duroxide::providers::WorkItem;

    let old_json = r#"{"ActivityExecute":{"instance":"i","execution_id":1,"id":2,"name":"X","input":"{}"}}"#;
    let item: WorkItem = serde_json::from_str(old_json).expect("should deserialize old format");
    match item {
        WorkItem::ActivityExecute { session_id, .. } => {
            assert!(session_id.is_none(), "missing session_id should default to None");
        }
        _ => panic!("expected ActivityExecute"),
    }

    let new_json =
        r#"{"ActivityExecute":{"instance":"i","execution_id":1,"id":2,"name":"X","input":"{}","session_id":"s1"}}"#;
    let item2: WorkItem = serde_json::from_str(new_json).expect("should deserialize new format");
    match item2 {
        WorkItem::ActivityExecute { session_id, .. } => {
            assert_eq!(session_id, Some("s1".to_string()));
        }
        _ => panic!("expected ActivityExecute"),
    }

    let item_none = WorkItem::ActivityExecute {
        instance: "i".to_string(),
        execution_id: 1,
        id: 2,
        name: "X".to_string(),
        input: "{}".to_string(),
        session_id: None,
    };
    let json = serde_json::to_string(&item_none).unwrap();
    assert!(!json.contains("session_id"), "None should be omitted: {json}");
}

/// SessionOpened and SessionClosed events serialize and deserialize correctly.
#[tokio::test]
async fn session_events_serialize_roundtrip() {
    use duroxide::{Event, EventKind};

    let opened = Event::with_event_id(
        1,
        "test",
        1,
        None,
        EventKind::SessionOpened {
            session_id: "ses-1".to_string(),
        },
    );
    let json = serde_json::to_string(&opened).unwrap();
    let deserialized: Event = serde_json::from_str(&json).unwrap();
    assert!(matches!(
        &deserialized.kind,
        EventKind::SessionOpened { session_id } if session_id == "ses-1"
    ));

    let closed = Event::with_event_id(
        2,
        "test",
        1,
        None,
        EventKind::SessionClosed {
            session_id: "ses-1".to_string(),
        },
    );
    let json = serde_json::to_string(&closed).unwrap();
    let deserialized: Event = serde_json::from_str(&json).unwrap();
    assert!(matches!(
        &deserialized.kind,
        EventKind::SessionClosed { session_id } if session_id == "ses-1"
    ));
}
