use duroxide::providers::Provider;
use std::sync::Arc;
mod common;
use duroxide::runtime::registry::ActivityRegistry;
use duroxide::runtime::{self};
use duroxide::{ActivityContext, OrchestrationContext, OrchestrationRegistry};
use std::sync::Arc as StdArc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tracing::info;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct AOnly {
    a: i32,
}

fn parse_activity_result(s: &Result<String, String>) -> Result<String, String> {
    s.clone()
}

async fn error_handling_compensation_on_ship_failure_with(store: StdArc<dyn Provider>) {
    let activity_registry = ActivityRegistry::builder()
        .register("Debit", |_ctx: ActivityContext, input: String| async move {
            if input == "fail" {
                Err("insufficient".to_string())
            } else {
                Ok(format!("debited:{input}"))
            }
        })
        .register("Ship", |_ctx: ActivityContext, input: String| async move {
            if input == "fail_ship" {
                Err("courier_down".to_string())
            } else {
                Ok("shipped".to_string())
            }
        })
        .register("Credit", |_ctx: ActivityContext, input: String| async move {
            Ok(format!("credited:{input}"))
        })
        .build();

    let orchestration = |ctx: OrchestrationContext, _input: String| async move {
        let deb = ctx.schedule_activity("Debit", "ok").into_activity().await;
        let deb = parse_activity_result(&deb);
        match deb {
            Err(e) => Ok(format!("debit_failed:{e}")),
            Ok(deb_val) => {
                let ship = ctx.schedule_activity("Ship", "fail_ship").into_activity().await;
                match parse_activity_result(&ship) {
                    Ok(_) => Ok("ok".to_string()),
                    Err(_) => {
                        let cred = ctx.schedule_activity("Credit", deb_val).into_activity().await.unwrap();
                        Ok(format!("rolled_back:{cred}"))
                    }
                }
            }
        }
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("ErrorHandlingCompensation", orchestration)
        .build();

    let rt =
        runtime::Runtime::start_with_store(store.clone(), Arc::new(activity_registry), orchestration_registry).await;
    let client = duroxide::Client::new(store.clone());
    client
        .start_orchestration("inst-err-ship-1", "ErrorHandlingCompensation", "")
        .await
        .unwrap();

    match client
        .wait_for_orchestration("inst-err-ship-1", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        duroxide::OrchestrationStatus::Completed { output } => {
            assert!(output.starts_with("rolled_back:credited:"));
        }
        duroxide::OrchestrationStatus::Failed { details } => {
            panic!("orchestration failed: {}", details.display_message())
        }
        _ => panic!("unexpected orchestration status"),
    }
    rt.shutdown(None).await;
}

#[tokio::test]
async fn error_handling_compensation_on_ship_failure_inmem() {
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;
    error_handling_compensation_on_ship_failure_with(store).await;
}

#[tokio::test]
async fn error_handling_compensation_on_ship_failure_fs() {
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;
    error_handling_compensation_on_ship_failure_with(store).await;
}

async fn error_handling_success_path_with(store: StdArc<dyn Provider>) {
    let activity_registry = ActivityRegistry::builder()
        .register("Debit", |_ctx: ActivityContext, input: String| async move {
            Ok(format!("debited:{input}"))
        })
        .register("Ship", |_ctx: ActivityContext, _input: String| async move {
            Ok("shipped".to_string())
        })
        .build();

    let orchestration = |ctx: OrchestrationContext, _input: String| async move {
        let deb = ctx.schedule_activity("Debit", "ok").into_activity().await;
        parse_activity_result(&deb).unwrap();
        let ship = ctx.schedule_activity("Ship", "ok").into_activity().await;
        parse_activity_result(&ship).unwrap();
        Ok("ok".to_string())
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("ErrorHandlingSuccess", orchestration)
        .build();

    let rt =
        runtime::Runtime::start_with_store(store.clone(), Arc::new(activity_registry), orchestration_registry).await;
    let client = duroxide::Client::new(store.clone());
    client
        .start_orchestration("inst-err-ok-1", "ErrorHandlingSuccess", "")
        .await
        .unwrap();

    match client
        .wait_for_orchestration("inst-err-ok-1", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        duroxide::OrchestrationStatus::Completed { output } => assert_eq!(output, "ok"),
        duroxide::OrchestrationStatus::Failed { details } => {
            panic!("orchestration failed: {}", details.display_message())
        }
        _ => panic!("unexpected orchestration status"),
    }
    rt.shutdown(None).await;
}

#[tokio::test]
async fn error_handling_success_path_inmem() {
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;
    error_handling_success_path_with(store).await;
}

#[tokio::test]
async fn error_handling_success_path_fs() {
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;
    error_handling_success_path_with(store).await;
}

async fn error_handling_early_debit_failure_with(store: StdArc<dyn Provider>) {
    let activity_registry = ActivityRegistry::builder()
        .register("Debit", |_ctx: ActivityContext, input: String| async move {
            Err(format!("bad:{input}"))
        })
        .register("Ship", |_ctx: ActivityContext, _input: String| async move {
            Ok("shipped".to_string())
        })
        .register("Credit", |_ctx: ActivityContext, _input: String| async move {
            Ok("credited".to_string())
        })
        .build();

    let orchestration = |ctx: OrchestrationContext, _input: String| async move {
        let deb = ctx.schedule_activity("Debit", "fail").into_activity().await;
        match deb {
            Err(e) => Ok(format!("debit_failed:{e}")),
            Ok(_) => unreachable!(),
        }
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("DebitFailureTest", orchestration)
        .build();

    let rt =
        runtime::Runtime::start_with_store(store.clone(), Arc::new(activity_registry), orchestration_registry).await;
    let client = duroxide::Client::new(store.clone());
    client
        .start_orchestration("inst-err-debit-1", "DebitFailureTest", "")
        .await
        .unwrap();

    match client
        .wait_for_orchestration("inst-err-debit-1", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        duroxide::OrchestrationStatus::Completed { output } => {
            assert!(output.starts_with("debit_failed:"));
        }
        duroxide::OrchestrationStatus::Failed { details } => {
            panic!("orchestration failed: {}", details.display_message())
        }
        _ => panic!("unexpected orchestration status"),
    }
    rt.shutdown(None).await;
}

#[tokio::test]
async fn error_handling_early_debit_failure_inmem() {
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;
    error_handling_early_debit_failure_with(store).await;
}

#[tokio::test]
async fn error_handling_early_debit_failure_fs() {
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;
    error_handling_early_debit_failure_with(store).await;
}

// 5) Unknown activity handler: should eventually poison after backoff attempts
async fn unknown_activity_fails_with(store: StdArc<dyn Provider>) {
    let activity_registry = ActivityRegistry::builder().build();
    let orchestration = |ctx: OrchestrationContext, _input: String| async move {
        match ctx.schedule_activity("Missing", "foo").into_activity().await {
            Ok(v) => Ok(format!("unexpected_ok:{v}")),
            Err(e) => Ok(format!("err={e}")),
        }
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("MissingActivityTest", orchestration)
        .build();

    // Use fast backoff for testing
    let options = duroxide::runtime::RuntimeOptions {
        max_attempts: 3,
        dispatcher_min_poll_interval: std::time::Duration::from_millis(10),
        unregistered_backoff: duroxide::runtime::UnregisteredBackoffConfig {
            base_delay: std::time::Duration::from_millis(10),
            max_delay: std::time::Duration::from_millis(50),
        },
        ..Default::default()
    };

    let rt = runtime::Runtime::start_with_options(
        store.clone(),
        Arc::new(activity_registry),
        orchestration_registry,
        options,
    )
    .await;
    let client = duroxide::Client::new(store.clone());
    client
        .start_orchestration("inst-unknown-act-1", "MissingActivityTest", "")
        .await
        .unwrap();

    match client
        .wait_for_orchestration("inst-unknown-act-1", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        duroxide::OrchestrationStatus::Failed { details } => {
            // Should fail with application error from activity failure/poison
            assert!(
                matches!(details, duroxide::ErrorDetails::Application { .. })
                    || matches!(details, duroxide::ErrorDetails::Poison { .. })
            );
        }
        duroxide::OrchestrationStatus::Completed { output } => {
            panic!("expected failure, got success: {output}");
        }
        _ => panic!("unexpected orchestration status"),
    }
    rt.shutdown(None).await;
}

#[tokio::test]
async fn unknown_activity_fails_fs() {
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;
    unknown_activity_fails_with(store).await;
}

// 6) Event after orchestration completion is ignored (no history change)
#[tokio::test]
async fn event_after_completion_is_ignored_fs() {
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;
    let activity_registry = ActivityRegistry::builder().build();

    let instance = "inst-post-complete-1";
    // Orchestration: subscribe and exit on first event
    let orchestration = |ctx: OrchestrationContext, _input: String| async move {
        let _ = ctx.schedule_wait("Once").into_event().await;
        Ok("done".to_string())
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("PostCompleteTest", orchestration)
        .build();

    let rt =
        runtime::Runtime::start_with_store(store.clone(), Arc::new(activity_registry), orchestration_registry).await;
    let store_for_wait = store.clone();
    let client_c = duroxide::Client::new(store.clone());
    let client = duroxide::Client::new(store.clone());
    tokio::spawn(async move {
        let _ = common::wait_for_subscription(store_for_wait, instance, "Once", 1000).await;
        let _ = client_c.raise_event(instance, "Once", "go").await;
    });
    client
        .start_orchestration(instance, "PostCompleteTest", "")
        .await
        .unwrap();

    match client
        .wait_for_orchestration(instance, std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        duroxide::OrchestrationStatus::Completed { output } => assert_eq!(output, "done"),
        duroxide::OrchestrationStatus::Failed { details } => {
            panic!("orchestration failed: {}", details.display_message())
        }
        _ => panic!("unexpected orchestration status"),
    }
    // Allow runtime to append OrchestrationCompleted terminal event
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    let before = store.read(instance).await.unwrap_or_default().len();

    // Raise another event after completion
    let _ = client.raise_event(instance, "Once", "late").await;
    // Give router a moment
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    let hist_after = store.read(instance).await.unwrap_or_default();
    assert_eq!(
        hist_after.len(),
        before,
        "post-completion event must not append history"
    );
    rt.shutdown(None).await;
}

// 7) Event raised before subscription after instance start is ignored
#[tokio::test]
async fn event_before_subscription_after_start_is_ignored() {
    // Use FS store for consistency
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;
    let activity_registry = ActivityRegistry::builder().build();
    let orchestration = |ctx: OrchestrationContext, _input: String| async move {
        info!("Orchestration started");
        // Delay before subscribing to simulate missing subscription window
        ctx.schedule_timer(Duration::from_millis(10)).into_timer().await;
        info!("Subscribing to event");
        // Subscribe, then wait for event with timeout
        let ev = ctx.schedule_wait("Evt");
        let to = ctx.schedule_timer(Duration::from_millis(1000));
        match ctx.select2(ev, to).await {
            (0, duroxide::DurableOutput::External(data)) => {
                info!("Event received: {}", data);
                Ok(data)
            }
            (1, duroxide::DurableOutput::Timer) => {
                info!("Timeout waiting for event");
                panic!("timeout waiting for Evt after subscription")
            }
            _ => unreachable!(),
        }
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("PreSubscriptionTest", orchestration)
        .build();

    let rt =
        runtime::Runtime::start_with_store(store.clone(), Arc::new(activity_registry), orchestration_registry).await;
    let client = duroxide::Client::new(store.clone());
    // Orchestration: delay, then subscribe
    let instance = "inst-pre-sub-drop-1";
    let client_c1 = duroxide::Client::new(store.clone());
    tokio::spawn(async move {
        info!("Raising early event");
        // Raise early before subscription exists (timer delays subscription)
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        let _ = client_c1.raise_event(instance, "Evt", "early").await;
    });
    let store_for_wait2 = store.clone();
    let client_c2 = duroxide::Client::new(store.clone());
    client
        .start_orchestration(instance, "PreSubscriptionTest", "")
        .await
        .unwrap();
    tokio::spawn(async move {
        let _ = common::wait_for_subscription(store_for_wait2.clone(), instance, "Evt", 1000).await;
        let _ = client_c2.raise_event(instance, "Evt", "late").await;
    });

    match client
        .wait_for_orchestration(instance, std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        duroxide::OrchestrationStatus::Completed { output } => assert_eq!(output, "late"),
        duroxide::OrchestrationStatus::Failed { details } => {
            panic!("orchestration failed: {}", details.display_message())
        }
        _ => panic!("unexpected orchestration status"),
    }
    rt.shutdown(None).await;
}

// 8) History cap exceeded triggers a hard error (no truncation) for both providers
async fn history_cap_exceeded_with(store: StdArc<dyn Provider>) {
    let activity_registry = ActivityRegistry::builder()
        .register(
            "Noop",
            |_ctx: ActivityContext, _in: String| async move { Ok(String::new()) },
        )
        .build();

    // Orchestration that schedules more than CAP events.
    // Each activity emits two events (Scheduled + Completed). With CAP=1024, 600 activities exceed.
    let orchestration = |ctx: OrchestrationContext, _input: String| async move {
        for i in 0..600u32 {
            let _ = ctx.schedule_activity("Noop", format!("{i}")).into_activity().await;
        }
        Ok("done".to_string())
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("HistoryCapTest", orchestration)
        .build();

    let rt =
        runtime::Runtime::start_with_store(store.clone(), Arc::new(activity_registry), orchestration_registry).await;
    let client = duroxide::Client::new(store.clone());
    client
        .start_orchestration("inst-cap-exceed", "HistoryCapTest", "")
        .await
        .unwrap();

    // Expect runtime to report Err result via waiter on append failure
    match client
        .wait_for_orchestration("inst-cap-exceed", std::time::Duration::from_secs(10))
        .await
    {
        Ok(duroxide::OrchestrationStatus::Failed { details: _ }) => {} // Expected failure due to history capacity
        Ok(duroxide::OrchestrationStatus::Completed { output }) => {
            panic!("expected failure due to history capacity, got: {output}")
        }
        Ok(_) => panic!("unexpected orchestration status"),
        Err(duroxide::ClientError::Timeout) => {
            // This is also acceptable - the orchestration may not be able to write a terminal event due to capacity
            // In this case, the polling JoinHandle should detect the persistence error
        }
        Err(_) => {
            // Other errors are also acceptable for capacity exceeded scenarios
        }
    }
    rt.shutdown(None).await;
}

#[tokio::test]
async fn history_cap_exceeded_inmem() {
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;
    history_cap_exceeded_with(store).await;
}

#[tokio::test]
async fn history_cap_exceeded_fs() {
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;
    history_cap_exceeded_with(store).await;
}

#[tokio::test]
async fn orchestration_immediate_fail_fs() {
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;
    let activity_registry = ActivityRegistry::builder().build();

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("AlwaysErr", |_ctx: OrchestrationContext, _| async move {
            Err("oops".to_string())
        })
        .build();

    let rt =
        runtime::Runtime::start_with_store(store.clone(), Arc::new(activity_registry), orchestration_registry).await;
    let client = duroxide::Client::new(store.clone());
    client
        .start_orchestration("inst-fail-imm", "AlwaysErr", "")
        .await
        .unwrap();

    match client
        .wait_for_orchestration("inst-fail-imm", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        duroxide::OrchestrationStatus::Failed { details: _ } => {} // Expected failure
        duroxide::OrchestrationStatus::Completed { output } => panic!("expected failure, got: {output}"),
        _ => panic!("unexpected orchestration status"),
    }

    // Check history for failure event
    let hist = client.read_execution_history("inst-fail-imm", 1).await.unwrap();
    // Expect OrchestrationStarted + OrchestrationFailed
    assert_eq!(hist.len(), 2);
    assert!(matches!(
        &hist.first().unwrap().kind,
        duroxide::EventKind::OrchestrationStarted { .. }
    ));
    assert!(matches!(
        &hist.last().unwrap().kind,
        duroxide::EventKind::OrchestrationFailed { .. }
    ));
    // Status API should report Failed with same error
    match client.get_orchestration_status("inst-fail-imm").await.unwrap() {
        duroxide::OrchestrationStatus::Failed { details } => {
            assert!(matches!(
                details,
                duroxide::ErrorDetails::Application {
                    kind: duroxide::AppErrorKind::OrchestrationFailed,
                    message,
                    ..
                } if message == "oops"
            ));
        }
        other => panic!("unexpected status: {other:?}"),
    }
    rt.shutdown(None).await;
}

#[tokio::test]
async fn orchestration_propagates_activity_failure_fs() {
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;
    let activity_registry = ActivityRegistry::builder()
        .register("Fail", |_ctx: ActivityContext, _in: String| async move {
            Err("bad".to_string())
        })
        .build();

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("PropagateFail", |ctx, _| async move {
            let r = ctx.schedule_activity("Fail", "x").into_activity().await;
            r.map(|_v| "ok".to_string())
        })
        .build();

    let rt =
        runtime::Runtime::start_with_store(store.clone(), Arc::new(activity_registry), orchestration_registry).await;
    let client = duroxide::Client::new(store.clone());
    client
        .start_orchestration("inst-fail-prop", "PropagateFail", "")
        .await
        .unwrap();

    match client
        .wait_for_orchestration("inst-fail-prop", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        duroxide::OrchestrationStatus::Failed { details } => {
            assert!(matches!(
                details,
                duroxide::ErrorDetails::Application {
                    kind: duroxide::AppErrorKind::OrchestrationFailed,
                    message,
                    ..
                } if message == "bad"
            ));
        }
        duroxide::OrchestrationStatus::Completed { output } => panic!("expected failure, got: {output}"),
        _ => panic!("unexpected orchestration status"),
    }

    // Check history for failure event
    let hist = client.read_execution_history("inst-fail-prop", 1).await.unwrap();
    assert!(matches!(
        &hist.last().unwrap().kind,
        duroxide::EventKind::OrchestrationFailed { .. }
    ));
    match client.get_orchestration_status("inst-fail-prop").await.unwrap() {
        duroxide::OrchestrationStatus::Failed { details } => {
            assert!(matches!(
                details,
                duroxide::ErrorDetails::Application {
                    kind: duroxide::AppErrorKind::OrchestrationFailed,
                    message,
                    ..
                } if message == "bad"
            ));
        }
        other => panic!("unexpected status: {other:?}"),
    }
    rt.shutdown(None).await;
}

#[tokio::test]
async fn typed_activity_decode_error_fs() {
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;
    // activity expects AOnly, returns stringified 'a'
    let activity_registry = ActivityRegistry::builder()
        .register_typed::<AOnly, String, _, _>("FmtA", |_ctx: ActivityContext, req| async move {
            Ok(format!("a={}", req.a))
        })
        .build();
    let orch = |ctx: OrchestrationContext, _in: String| async move {
        // Pass invalid payload (not JSON for AOnly)
        let res = ctx.schedule_activity("FmtA", "not-json").into_activity().await;
        // The activity worker decodes input; expect Err
        assert!(res.is_err());
        Ok("ok".to_string())
    };
    let orchestration_registry = OrchestrationRegistry::builder()
        .register("BadInputToTypedActivity", orch)
        .build();
    let rt =
        runtime::Runtime::start_with_store(store.clone(), Arc::new(activity_registry), orchestration_registry).await;
    let client = duroxide::Client::new(store.clone());
    client
        .start_orchestration("inst-typed-bad", "BadInputToTypedActivity", "")
        .await
        .unwrap();

    let status = client
        .wait_for_orchestration("inst-typed-bad", std::time::Duration::from_secs(5))
        .await
        .unwrap();
    let output = match status {
        duroxide::OrchestrationStatus::Completed { output } => output,
        duroxide::OrchestrationStatus::Failed { details } => {
            panic!("orchestration failed: {}", details.display_message())
        }
        _ => panic!("unexpected orchestration status"),
    };
    assert_eq!(output, "ok");
    rt.shutdown(None).await;
}

#[tokio::test]
async fn typed_event_decode_error_fs() {
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;
    let activity_registry = ActivityRegistry::builder().build();
    let orch = |ctx: OrchestrationContext, _in: String| async move {
        // attempt to decode event into AOnly
        let fut = ctx.schedule_wait_typed::<AOnly>("Evt").into_event_typed::<AOnly>();
        Ok(
            match futures::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(fut)).await {
                Ok(v) => {
                    // If it somehow decodes, convert to string
                    let _val: AOnly = v;
                    "ok".to_string()
                }
                Err(_) => "decode_err".to_string(),
            },
        )
    };
    let orchestration_registry = OrchestrationRegistry::builder()
        .register_typed::<String, String, _, _>("TypedEvt", orch)
        .build();
    let rt =
        runtime::Runtime::start_with_store(store.clone(), Arc::new(activity_registry), orchestration_registry).await;
    let client = duroxide::Client::new(store.clone());
    let store_for_wait = store.clone();
    tokio::spawn(async move {
        let _ = crate::common::wait_for_subscription(store_for_wait, "inst-typed-evt", "Evt", 1000).await;
        // invalid payload for AOnly
        let _ = client.raise_event("inst-typed-evt", "Evt", "not-json").await;
    });
    duroxide::Client::new(store.clone())
        .start_orchestration_typed::<String>("inst-typed-evt", "TypedEvt", "".to_string())
        .await
        .unwrap();

    let status = duroxide::Client::new(store.clone())
        .wait_for_orchestration_typed::<String>("inst-typed-evt", std::time::Duration::from_secs(5))
        .await
        .unwrap();
    let output = match status {
        Ok(output) => output,
        Err(error) => panic!("orchestration failed: {error}"),
    };
    assert_eq!(output, "decode_err");
    rt.shutdown(None).await;
}
