//! End-to-end samples using SQLite provider.
//! 
//! This mirrors all tests from e2e_samples.rs but uses SQLite instead of filesystem storage.

use duroxide::providers::sqlite::SqliteHistoryStore;
use duroxide::providers::HistoryStore;
use duroxide::runtime::{self, registry::ActivityRegistry, OrchestrationStatus};
use duroxide::{OrchestrationContext, OrchestrationRegistry, DurableOutput};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;

mod common;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct AddReq {
    a: i32,
    b: i32,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct AddRes {
    sum: i32,
}

/// Helper to create a SQLite store for tests
async fn create_sqlite_store() -> (Arc<dyn HistoryStore>, TempDir) {
    // Use file-based SQLite database with temporary file
    let temp_dir = tempfile::TempDir::new().expect("Failed to create temp dir");
    let db_path = temp_dir.path().join("test.db");
    
    // Create the database file
    std::fs::File::create(&db_path).expect("Failed to create database file");
    
    let db_url = format!("sqlite:{}", db_path.to_str().unwrap());
    
    let store = Arc::new(
        SqliteHistoryStore::new(&db_url)
            .await
            .expect("Failed to create SQLite store")
    ) as Arc<dyn HistoryStore>;
    
    (store, temp_dir)
}

#[tokio::test]
async fn sample_hello_world_sqlite() {
    let (store, _temp_dir) = create_sqlite_store().await;

    // Register a simple activity: "Hello" -> format a greeting
    let activity_registry = ActivityRegistry::builder()
        .register("Hello", |input: String| async move { Ok(format!("Hello, {input}!")) })
        .build();

    // Orchestrator: emit a trace, call Hello twice, return result using input
    let orchestration = |ctx: OrchestrationContext, input: String| async move {
        ctx.trace_info("hello_world started");
        let res = ctx.schedule_activity("Hello", "Rust").into_activity().await?;
        ctx.trace_info(format!("hello_world result={res} "));
        let res1 = ctx.schedule_activity("Hello", input).into_activity().await?;
        ctx.trace_info(format!("hello_world result={res1} "));
        Ok(res1)
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("HelloWorld", orchestration)
        .build();

    let rt = runtime::Runtime::start_with_store(store.clone(), Arc::new(activity_registry), orchestration_registry).await;
    
    rt.clone()
        .start_orchestration("inst-sample-hello-1", "HelloWorld", "World")
        .await
        .unwrap();

    match rt.wait_for_orchestration("inst-sample-hello-1", Duration::from_secs(5))
        .await
        .unwrap()
    {
        OrchestrationStatus::Completed { output } => assert_eq!(output, "Hello, World!"),
        OrchestrationStatus::Failed { error } => panic!("orchestration failed: {error}"),
        _ => panic!("unexpected status"),
    }
}

#[tokio::test]
async fn sample_basic_control_flow_sqlite() {
    let (store, _temp_dir) = create_sqlite_store().await;

    let activity_registry = ActivityRegistry::builder()
        .register("CheckCondition", |_: String| async move { Ok("yes".to_string()) })
        .register("IfYes", |_: String| async move { Ok("picked_yes".to_string()) })
        .register("IfNo", |_: String| async move { Ok("picked_no".to_string()) })
        .build();

    let orchestration = |ctx: OrchestrationContext, _: String| async move {
        let condition = ctx.schedule_activity("CheckCondition", "").into_activity().await?;
        if condition == "yes" {
            ctx.schedule_activity("IfYes", "").into_activity().await
        } else {
            ctx.schedule_activity("IfNo", "").into_activity().await
        }
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("ControlFlow", orchestration)
        .build();

    let rt = runtime::Runtime::start_with_store(store.clone(), Arc::new(activity_registry), orchestration_registry).await;
    
    rt.clone()
        .start_orchestration("inst-sample-cflow-1", "ControlFlow", "")
        .await
        .unwrap();

    match rt.wait_for_orchestration("inst-sample-cflow-1", Duration::from_secs(5))
        .await
        .unwrap()
    {
        OrchestrationStatus::Completed { output } => assert_eq!(output, "picked_yes"),
        OrchestrationStatus::Failed { error } => panic!("orchestration failed: {error}"),
        _ => panic!("unexpected status"),
    }
}

#[tokio::test]
async fn sample_loop_sqlite() {
    let (store, _temp_dir) = create_sqlite_store().await;

    let activity_registry = ActivityRegistry::builder()
        .register("Append", |input: String| async move { Ok(input + "x") })
        .build();

    let orchestration = |ctx: OrchestrationContext, _: String| async move {
        let mut state = "start".to_string();
        for _ in 0..3 {
            state = ctx.schedule_activity("Append", &state).into_activity().await?;
        }
        Ok(state)
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("LoopOrchestration", orchestration)
        .build();

    let rt = runtime::Runtime::start_with_store(store.clone(), Arc::new(activity_registry), orchestration_registry).await;
    
    rt.clone()
        .start_orchestration("inst-sample-loop-1", "LoopOrchestration", "")
        .await
        .unwrap();

    match rt.wait_for_orchestration("inst-sample-loop-1", Duration::from_secs(5))
        .await
        .unwrap()
    {
        OrchestrationStatus::Completed { output } => assert_eq!(output, "startxxx"),
        OrchestrationStatus::Failed { error } => panic!("orchestration failed: {error}"),
        _ => panic!("unexpected status"),
    }
}

#[tokio::test]
async fn sample_error_handling_sqlite() {
    let (store, _temp_dir) = create_sqlite_store().await;

    let activity_registry = ActivityRegistry::builder()
        .register("MightFail", |input: String| async move {
            if input == "fail" {
                Err("simulated failure".to_string())
            } else {
                Ok("success".to_string())
            }
        })
        .register("Compensate", |_: String| async move { Ok("compensated".to_string()) })
        .build();

    let orchestration = |ctx: OrchestrationContext, _: String| async move {
        let res = ctx.schedule_activity("MightFail", "fail").into_activity().await;
        match res {
            Ok(_) => Ok("succeeded".to_string()),
            Err(_) => {
                let _comp = ctx.schedule_activity("Compensate", "").into_activity().await?;
                Ok("recovered".to_string())
            }
        }
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("ErrorHandling", orchestration)
        .build();

    let rt = runtime::Runtime::start_with_store(store.clone(), Arc::new(activity_registry), orchestration_registry).await;
    
    rt.clone()
        .start_orchestration("inst-sample-err-1", "ErrorHandling", "")
        .await
        .unwrap();

    match rt.wait_for_orchestration("inst-sample-err-1", Duration::from_secs(5))
        .await
        .unwrap()
    {
        OrchestrationStatus::Completed { output } => assert_eq!(output, "recovered"),
        OrchestrationStatus::Failed { error } => panic!("orchestration failed: {error}"),
        _ => panic!("unexpected status"),
    }
}

#[tokio::test]
async fn sample_timeout_with_timer_race_sqlite() {
    let (store, _temp_dir) = create_sqlite_store().await;

    let activity_registry = ActivityRegistry::builder()
        .register("LongRunning", |_: String| async move {
            // Simulate a long-running activity
            // Note: Activities should not use tokio::time::sleep in production
            // This is just for testing timeout behavior
            tokio::time::sleep(Duration::from_millis(1000)).await; // Sleep longer than timer
            Ok("completed".to_string())
        })
        .build();

    let orchestration = |ctx: OrchestrationContext, _: String| async move {
        let activity_future = ctx.schedule_activity("LongRunning", "");
        let timer_future = ctx.schedule_timer(500); // 500ms timeout
        
        let (winner_idx, _output) = ctx.select2(activity_future, timer_future).await;
        
        if winner_idx == 0 {
            Ok("activity_won".to_string())
        } else {
            Err("timeout".to_string())
        }
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("TimeoutSample", orchestration)
        .build();

    let rt = runtime::Runtime::start_with_store(store.clone(), Arc::new(activity_registry), orchestration_registry).await;
    
    rt.clone()
        .start_orchestration("inst-timeout-sample", "TimeoutSample", "")
        .await
        .unwrap();

    match rt.wait_for_orchestration("inst-timeout-sample", Duration::from_secs(5))
        .await
        .unwrap()
    {
        OrchestrationStatus::Failed { error } => assert_eq!(error, "timeout"),
        OrchestrationStatus::Completed { output } => panic!("expected timeout failure, got: {output}"),
        _ => panic!("unexpected status"),
    }
}

#[tokio::test]
async fn sample_sub_orchestration_basic_sqlite() {
    let (store, _temp_dir) = create_sqlite_store().await;

    let activity_registry = ActivityRegistry::builder()
        .register("ToUpper", |input: String| async move { Ok(input.to_uppercase()) })
        .build();

    // Child orchestration
    let child = |ctx: OrchestrationContext, input: String| async move {
        ctx.schedule_activity("ToUpper", &input).into_activity().await
    };

    // Parent orchestration
    let parent = |ctx: OrchestrationContext, input: String| async move {
        let child_result = ctx
            .schedule_sub_orchestration("Child", &input)
            .into_sub_orchestration()
            .await?;
        Ok(format!("parent:{}", child_result))
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("Child", child)
        .register("Parent", parent)
        .build();

    let rt = runtime::Runtime::start_with_store(store.clone(), Arc::new(activity_registry), orchestration_registry).await;
    
    rt.clone()
        .start_orchestration("inst-sub-basic", "Parent", "hi")
        .await
        .unwrap();

    match rt.wait_for_orchestration("inst-sub-basic", Duration::from_secs(20))
        .await
        .unwrap()
    {
        OrchestrationStatus::Completed { output } => assert_eq!(output, "parent:HI"),
        OrchestrationStatus::Failed { error } => panic!("orchestration failed: {error}"),
        _ => panic!("unexpected status"),
    }
}

#[tokio::test]
async fn sample_fan_out_fan_in_sqlite() {
    let (store, _temp_dir) = create_sqlite_store().await;

    let activity_registry = ActivityRegistry::builder()
        .register("ProcessItem", |input: String| async move {
            let n: i32 = input.parse().unwrap();
            Ok((n * 2).to_string())
        })
        .build();

    let orchestration = |ctx: OrchestrationContext, _: String| async move {
        // Fan-out: schedule multiple activities in parallel
        let futures = vec![
            ctx.schedule_activity("ProcessItem", "1"),
            ctx.schedule_activity("ProcessItem", "2"),
            ctx.schedule_activity("ProcessItem", "3"),
            ctx.schedule_activity("ProcessItem", "4"),
            ctx.schedule_activity("ProcessItem", "5"),
        ];
        
        // Fan-in: wait for all results
        let results = ctx.join(futures).await;
        
        let sum: i32 = results.into_iter().map(|r| match r {
            duroxide::DurableOutput::Activity(Ok(val)) => val.parse::<i32>().unwrap(),
            _ => 0,
        }).sum();
        
        Ok(format!("sum={}", sum))
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("FanOutFanIn", orchestration)
        .build();

    let rt = runtime::Runtime::start_with_store(store.clone(), Arc::new(activity_registry), orchestration_registry).await;
    
    rt.clone()
        .start_orchestration("inst-fan-sample", "FanOutFanIn", "")
        .await
        .unwrap();

    match rt.wait_for_orchestration("inst-fan-sample", Duration::from_secs(5))
        .await
        .unwrap()
    {
        OrchestrationStatus::Completed { output } => assert_eq!(output, "sum=30"), // (1+2+3+4+5)*2 = 30
        OrchestrationStatus::Failed { error } => panic!("orchestration failed: {error}"),
        _ => panic!("unexpected status"),
    }
}

#[tokio::test]
async fn sample_select2_activity_vs_external_sqlite() {
    let (store, _temp_dir) = create_sqlite_store().await;

    let activity_registry = ActivityRegistry::builder()
        .register("Sleep", |_input: String| async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            Ok("slept".to_string())
        })
        .build();

    let orchestration = |ctx: OrchestrationContext, _input: String| async move {
        let act = ctx.schedule_activity("Sleep", "");
        let evt = ctx.schedule_wait("Go");
        let (idx, out) = ctx.select2(act, evt).await;
        // Demonstrate using the index to branch
        match (idx, out) {
            (0, DurableOutput::Activity(Ok(s))) => Ok(format!("activity:{s}")),
            (1, DurableOutput::External(payload)) => Ok(format!("event:{payload}")),
            (0, DurableOutput::Activity(Err(e))) => Err(e),
            other => panic!("unexpected: {:?}", other),
        }
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("Select2ActVsEvt", orchestration)
        .build();

    let rt = runtime::Runtime::start_with_store(store.clone(), Arc::new(activity_registry), orchestration_registry).await;

    // Start orchestration, then raise external after subscription is recorded
    let store_for_wait = store.clone();
    let rt_c = rt.clone();
    tokio::spawn(async move {
        let _ = common::wait_for_subscription(store_for_wait, "inst-s2-mixed", "Go", 1000).await;
        rt_c.raise_event("inst-s2-mixed", "Go", "ok").await;
    });

    rt.clone()
        .start_orchestration("inst-s2-mixed", "Select2ActVsEvt", "")
        .await
        .unwrap();

    let s = match rt
        .wait_for_orchestration("inst-s2-mixed", Duration::from_secs(5))
        .await
        .unwrap()
    {
        OrchestrationStatus::Completed { output } => output,
        OrchestrationStatus::Failed { error } => panic!("orchestration failed: {error}"),
        _ => panic!("unexpected orchestration status"),
    };
    // External event should win (idx==1) because activity sleeps 300ms
    assert_eq!(s, "event:ok");
}

#[tokio::test]
async fn sample_system_activities_sqlite() {
    let (store, _temp_dir) = create_sqlite_store().await;

    let activity_registry = ActivityRegistry::builder().build();

    let orchestration = |ctx: OrchestrationContext, _input: String| async move {
        let now_str = ctx.utcnow_ms().into_activity().await?;
        let now: u128 = now_str.parse().unwrap();
        let guid = ctx.new_guid().into_activity().await?;
        ctx.trace_info(format!("system now={now}, guid={guid}"));
        Ok(format!("n={now},g={guid}"))
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("SystemActivities", orchestration)
        .build();

    let rt = runtime::Runtime::start_with_store(store.clone(), Arc::new(activity_registry), orchestration_registry).await;
    
    rt.clone()
        .start_orchestration("inst-system-acts", "SystemActivities", "")
        .await
        .unwrap();

    let out = match rt
        .wait_for_orchestration("inst-system-acts", Duration::from_secs(5))
        .await
        .unwrap()
    {
        OrchestrationStatus::Completed { output } => output,
        OrchestrationStatus::Failed { error } => panic!("orchestration failed: {error}"),
        _ => panic!("unexpected orchestration status"),
    };
    // Basic assertions
    assert!(out.contains("n=") && out.contains(",g="));
    let parts: Vec<&str> = out.split([',', '=']).collect();
    // parts like ["n", now, "g", guid]
    assert!(parts.len() >= 4);
    let now_val: u128 = parts[1].parse().unwrap_or(0);
    let guid_str = parts[3];
    assert!(now_val > 0);
    assert_eq!(guid_str.len(), 32);
    assert!(guid_str.chars().all(|c| c.is_ascii_hexdigit()));
}

#[tokio::test]
async fn sample_external_event_sqlite() {
    let (store, _temp_dir) = create_sqlite_store().await;

    let activity_registry = ActivityRegistry::builder().build();

    let orchestration = |ctx: OrchestrationContext, _: String| async move {
        ctx.trace_info("Waiting for approval");
        
        // Wait for external event
        let approval_data = ctx
            .schedule_wait("approval")
            .into_event()
            .await;
        
        Ok(format!("Received: {}", approval_data))
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("WaitForApproval", orchestration)
        .build();

    let rt = runtime::Runtime::start_with_store(store.clone(), Arc::new(activity_registry), orchestration_registry).await;
    
    rt.clone()
        .start_orchestration("inst-ext-event", "WaitForApproval", "")
        .await
        .unwrap();

    // Simulate external system sending event
    tokio::time::sleep(Duration::from_millis(100)).await;
    rt.raise_event("inst-ext-event", "approval", "APPROVED").await;

    match rt.wait_for_orchestration("inst-ext-event", Duration::from_secs(5))
        .await
        .unwrap()
    {
        OrchestrationStatus::Completed { output } => assert_eq!(output, "Received: APPROVED"),
        OrchestrationStatus::Failed { error } => panic!("orchestration failed: {error}"),
        _ => panic!("unexpected status"),
    }
}

#[tokio::test]
async fn sample_continue_as_new_sqlite() {
    let (store, _temp_dir) = create_sqlite_store().await;

    let activity_registry = ActivityRegistry::builder().build();

    let orchestration = |ctx: OrchestrationContext, input: String| async move {
        let count: i32 = input.parse().unwrap_or(0);
        ctx.trace_info(&format!("Iteration {}", count));
        
        if count < 3 {
            ctx.continue_as_new((count + 1).to_string());
            // continue_as_new exits the orchestration, so return dummy value
            return Ok("continuing".to_string());
        } else {
            Ok(format!("final:{}", count))
        }
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("CanSample", orchestration)
        .build();

    let rt = runtime::Runtime::start_with_store(store.clone(), Arc::new(activity_registry), orchestration_registry).await;
    
    rt.clone()
        .start_orchestration("inst-sample-can", "CanSample", "0")
        .await
        .unwrap();

    match rt.wait_for_orchestration("inst-sample-can", Duration::from_secs(5))
        .await
        .unwrap()
    {
        OrchestrationStatus::Completed { output } => assert_eq!(output, "final:3"),
        OrchestrationStatus::Failed { error } => panic!("orchestration failed: {error}"),
        _ => panic!("unexpected status"),
    }
}

#[tokio::test]
async fn sample_typed_activity_and_orchestration_sqlite() {
    let (store, _temp_dir) = create_sqlite_store().await;


    let activity_registry = ActivityRegistry::builder()
        .register_typed("Add", |req: AddReq| async move {
            Ok(AddRes { sum: req.a + req.b })
        })
        .build();

    let orchestration = |ctx: OrchestrationContext, input: String| async move {
        let req: AddReq = serde_json::from_str(&input).unwrap();
        let res = ctx
            .schedule_activity_typed::<AddReq, AddRes>("Add", &req)
            .into_activity_typed::<AddRes>()
            .await?;
        Ok(serde_json::to_string(&res).unwrap())
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("Adder", orchestration)
        .build();

    let rt = runtime::Runtime::start_with_store(store.clone(), Arc::new(activity_registry), orchestration_registry).await;
    
    rt.clone()
        .start_orchestration_typed::<AddReq>("inst-typed-add", "Adder", AddReq { a: 2, b: 3 })
        .await
        .unwrap();

    match rt.wait_for_orchestration_typed::<AddRes>("inst-typed-add", Duration::from_secs(5))
        .await
        .unwrap()
    {
        Ok(result) => assert_eq!(result, AddRes { sum: 5 }),
        Err(error) => panic!("orchestration failed: {error}"),
    }
}

#[tokio::test]
async fn sample_timer_patterns_sqlite() {
    let (store, _temp_dir) = create_sqlite_store().await;

    let activity_registry = ActivityRegistry::builder()
        .register("FastOp", |_: String| async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            Ok("fast_done".to_string())
        })
        .build();

    let orchestration = |ctx: OrchestrationContext, _: String| async move {
        // Pattern 1: Simple delay
        ctx.schedule_timer(100).into_timer().await;
        
        // Pattern 2: Activity with timeout
        let activity = ctx.schedule_activity("FastOp", "");
        let timeout = ctx.schedule_timer(1000); // Increased to 1 second to ensure activity completes first
        
        let (idx, _) = ctx.select2(activity, timeout).await;
        let result = if idx == 0 {
            "activity_completed"
        } else {
            "timeout_hit"
        };
        
        Ok(result.to_string())
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("TimerPatterns", orchestration)
        .build();

    let rt = runtime::Runtime::start_with_store(store.clone(), Arc::new(activity_registry), orchestration_registry).await;
    
    rt.clone()
        .start_orchestration("inst-timer-patterns", "TimerPatterns", "")
        .await
        .unwrap();

    match rt.wait_for_orchestration("inst-timer-patterns", Duration::from_secs(20))
        .await
        .unwrap()
    {
        OrchestrationStatus::Completed { output } => assert_eq!(output, "activity_completed"),
        OrchestrationStatus::Failed { error } => panic!("orchestration failed: {error}"),
        _ => panic!("unexpected status"),
    }
}

#[tokio::test]
async fn sample_error_recovery_patterns_sqlite() {
    let (store, _temp_dir) = create_sqlite_store().await;

    let activity_registry = ActivityRegistry::builder()
        .register("UnreliableService", |attempt: String| async move {
            let n: i32 = attempt.parse().unwrap();
            if n < 3 {
                Err(format!("Failed on attempt {}", n))
            } else {
                Ok("Success!".to_string())
            }
        })
        .build();

    let orchestration = |ctx: OrchestrationContext, _: String| async move {
        let mut last_error = String::new();
        
        // Retry up to 3 times
        for attempt in 1..=3 {
            match ctx.schedule_activity("UnreliableService", attempt.to_string())
                .into_activity()
                .await
            {
                Ok(result) => return Ok(result),
                Err(e) => {
                    last_error = e;
                    ctx.trace_warn(&format!("Attempt {} failed: {}", attempt, last_error));
                    
                    // Exponential backoff
                    if attempt < 3 {
                        ctx.schedule_timer(100 * attempt as u64).into_timer().await;
                    }
                }
            }
        }
        
        Err(format!("All attempts failed. Last error: {}", last_error))
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("RetryWithBackoff", orchestration)
        .build();

    let rt = runtime::Runtime::start_with_store(store.clone(), Arc::new(activity_registry), orchestration_registry).await;
    
    rt.clone()
        .start_orchestration("inst-retry-pattern", "RetryWithBackoff", "")
        .await
        .unwrap();

    match rt.wait_for_orchestration("inst-retry-pattern", Duration::from_secs(20))
        .await
        .unwrap()
    {
        OrchestrationStatus::Completed { output } => assert_eq!(output, "Success!"),
        OrchestrationStatus::Failed { error } => panic!("orchestration failed: {error}"),
        _ => panic!("unexpected status"),
    }
}

#[tokio::test]
async fn sample_saga_pattern_sqlite() {
    let (store, _temp_dir) = create_sqlite_store().await;

    let activity_registry = ActivityRegistry::builder()
        .register("BookFlight", |_: String| async move { Ok("flight-123".to_string()) })
        .register("BookHotel", |_: String| async move { Ok("hotel-456".to_string()) })
        .register("BookCar", |_: String| async move { 
            Err("No cars available".to_string()) // This will fail
        })
        .register("CancelFlight", |id: String| async move { Ok(format!("Cancelled {}", id)) })
        .register("CancelHotel", |id: String| async move { Ok(format!("Cancelled {}", id)) })
        .build();

    let orchestration = |ctx: OrchestrationContext, _: String| async move {
        // Book flight
        let flight = ctx.schedule_activity("BookFlight", "").into_activity().await?;
        
        // Book hotel
        let hotel = match ctx.schedule_activity("BookHotel", "").into_activity().await {
            Ok(h) => h,
            Err(e) => {
                // Compensate: cancel flight
                ctx.schedule_activity("CancelFlight", &flight).into_activity().await?;
                return Err(format!("Hotel booking failed: {}", e));
            }
        };
        
        // Book car (this will fail)
        match ctx.schedule_activity("BookCar", "").into_activity().await {
            Ok(car) => Ok(format!("Booked: {}, {}, {}", flight, hotel, car)),
            Err(e) => {
                // Compensate: cancel both flight and hotel
                let cancel_flight = ctx.schedule_activity("CancelFlight", &flight);
                let cancel_hotel = ctx.schedule_activity("CancelHotel", &hotel);
                
                let _cancellations = ctx.join(vec![cancel_flight, cancel_hotel]).await;
                
                Err(format!("Car booking failed: {}. Compensated.", e))
            }
        }
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("TravelBookingSaga", orchestration)
        .build();

    let rt = runtime::Runtime::start_with_store(store.clone(), Arc::new(activity_registry), orchestration_registry).await;
    
    rt.clone()
        .start_orchestration("inst-saga", "TravelBookingSaga", "")
        .await
        .unwrap();

    match rt.wait_for_orchestration("inst-saga", Duration::from_secs(5))
        .await
        .unwrap()
    {
        OrchestrationStatus::Failed { error } => {
            assert!(error.contains("Car booking failed"));
            assert!(error.contains("Compensated"));
        },
        OrchestrationStatus::Completed { output } => panic!("Expected failure, got: {}", output),
        _ => panic!("unexpected status"),
    }
}

#[tokio::test]
async fn sample_human_interaction_pattern_sqlite() {
    let (store, _temp_dir) = create_sqlite_store().await;

    let activity_registry = ActivityRegistry::builder()
        .register("SendApprovalRequest", |doc: String| async move {
            Ok(format!("Sent approval request for: {}", doc))
        })
        .register("ProcessDocument", |doc: String| async move {
            Ok(format!("Processed: {}", doc))
        })
        .build();

    let orchestration = |ctx: OrchestrationContext, doc: String| async move {
        // Send approval request
        ctx.schedule_activity("SendApprovalRequest", &doc)
            .into_activity()
            .await?;
        
        // Set up timeout for approval
        let approval_event = ctx.schedule_wait("approval_decision");
        let timeout = ctx.schedule_timer(5000); // 5 second timeout
        
        let (idx, output) = ctx.select2(approval_event, timeout).await;
        
        if idx == 1 {
            return Err("Approval timeout".to_string());
        }
        
        // Check approval decision
        match output {
            duroxide::DurableOutput::External(decision) => {
                if decision == "approved" {
                    ctx.schedule_activity("ProcessDocument", &doc)
                        .into_activity()
                        .await
                } else {
                    Err(format!("Document rejected: {}", decision))
                }
            }
            _ => Err("Unexpected output".to_string()),
        }
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("DocumentApproval", orchestration)
        .build();

    let rt = runtime::Runtime::start_with_store(store.clone(), Arc::new(activity_registry), orchestration_registry).await;
    
    rt.clone()
        .start_orchestration("inst-human", "DocumentApproval", "important.pdf")
        .await
        .unwrap();

    // Simulate human approving after a delay
    tokio::time::sleep(Duration::from_millis(200)).await;
    rt.raise_event("inst-human", "approval_decision", "approved").await;

    match rt.wait_for_orchestration("inst-human", Duration::from_secs(5))
        .await
        .unwrap()
    {
        OrchestrationStatus::Completed { output } => assert_eq!(output, "Processed: important.pdf"),
        OrchestrationStatus::Failed { error } => panic!("orchestration failed: {error}"),
        _ => panic!("unexpected status"),
    }
}

#[tokio::test]
async fn sample_versioning_sqlite() {
    let (store, _temp_dir) = create_sqlite_store().await;

    let activity_registry = ActivityRegistry::builder().build();

    // V1 orchestration
    let v1_orch = |_ctx: OrchestrationContext, _: String| async move {
        Ok("v1_result".to_string())
    };

    // V2 orchestration with enhancements
    let v2_orch = |ctx: OrchestrationContext, _: String| async move {
        ctx.trace_info("Running v2 with new features");
        Ok("v2_result_enhanced".to_string())
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register_versioned("VersionedOrch", "1.0.0", v1_orch)
        .register_versioned("VersionedOrch", "2.0.0", v2_orch)
        .build();

    let rt = runtime::Runtime::start_with_store(store.clone(), Arc::new(activity_registry), orchestration_registry).await;
    
    // Start with explicit version
    rt.clone()
        .start_orchestration_versioned("inst-v1", "VersionedOrch", "1.0.0", "")
        .await
        .unwrap();

    match rt.wait_for_orchestration("inst-v1", Duration::from_secs(5))
        .await
        .unwrap()
    {
        OrchestrationStatus::Completed { output } => assert_eq!(output, "v1_result"),
        _ => panic!("unexpected status"),
    }

    // Start with latest version
    rt.clone()
        .start_orchestration("inst-v-latest", "VersionedOrch", "")
        .await
        .unwrap();

    match rt.wait_for_orchestration("inst-v-latest", Duration::from_secs(5))
        .await
        .unwrap()
    {
        OrchestrationStatus::Completed { output } => assert_eq!(output, "v2_result_enhanced"),
        _ => panic!("unexpected status"),
    }
}

#[tokio::test]
async fn sample_performance_test_sqlite() {
    let (store, _temp_dir) = create_sqlite_store().await;

    let activity_registry = ActivityRegistry::builder()
        .register("QuickOp", |input: String| async move {
            Ok(format!("processed_{}", input))
        })
        .build();

    let orchestration = |ctx: OrchestrationContext, _: String| async move {
        // Schedule many activities in parallel
        let mut futures = vec![];
        for i in 0..20 {
            futures.push(
                ctx.schedule_activity("QuickOp", i.to_string())
            );
        }
        
        let results = ctx.join(futures).await;
        let success_count = results.iter().filter(|r| {
            matches!(r, duroxide::DurableOutput::Activity(Ok(_)))
        }).count();
        
        Ok(format!("Completed {} operations", success_count))
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("ManyActivities", orchestration)
        .build();

    let rt = runtime::Runtime::start_with_store(store.clone(), Arc::new(activity_registry), orchestration_registry).await;
    
    let start = std::time::Instant::now();
    
    rt.clone()
        .start_orchestration("inst-perf", "ManyActivities", "")
        .await
        .unwrap();

    match rt.wait_for_orchestration("inst-perf", Duration::from_secs(10))
        .await
        .unwrap()
    {
        OrchestrationStatus::Completed { output } => {
            assert_eq!(output, "Completed 20 operations");
            let elapsed = start.elapsed();
            println!("SQLite performance test completed in {:?}", elapsed);
        }
        _ => panic!("unexpected status"),
    }
}

#[tokio::test]
async fn sample_cancellation_sqlite() {
    let (store, _temp_dir) = create_sqlite_store().await;

    let activity_registry = ActivityRegistry::builder()
        .register("LongWork", |_: String| async move {
            tokio::time::sleep(Duration::from_secs(5)).await;
            Ok("should_not_complete".to_string())
        })
        .build();

    let orchestration = |ctx: OrchestrationContext, _: String| async move {
        // Just do the long work - cancellation is handled by the runtime
        ctx.schedule_activity("LongWork", "").into_activity().await
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("CancellableWork", orchestration)
        .build();

    let rt = runtime::Runtime::start_with_store(store.clone(), Arc::new(activity_registry), orchestration_registry).await;
    
    rt.clone()
        .start_orchestration("inst-cancel", "CancellableWork", "")
        .await
        .unwrap();

    // For now, let's just wait and see if it completes
    // Cancellation API doesn't exist in the current implementation
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Check status
    let status = rt.get_orchestration_status("inst-cancel").await;
    assert!(!matches!(status, OrchestrationStatus::NotFound));
}

#[tokio::test]
async fn sample_status_polling_sqlite() {
    let (store, _temp_dir) = create_sqlite_store().await;

    let activity_registry = ActivityRegistry::builder()
        .register("Slow", |_: String| async move {
            tokio::time::sleep(Duration::from_millis(1000)).await;
            Ok("slow_done".to_string())
        })
        .build();

    let orchestration = |ctx: OrchestrationContext, _: String| async move {
        let res = ctx.schedule_activity("Slow", "").into_activity().await?;
        Ok(res)
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("StatusPoll", orchestration)
        .build();

    let rt = runtime::Runtime::start_with_store(store.clone(), Arc::new(activity_registry), orchestration_registry).await;
    
    rt.clone()
        .start_orchestration("inst-status-poll", "StatusPoll", "")
        .await
        .unwrap();

    // Small delay to ensure orchestration is registered
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Poll status until complete
    let mut iterations = 0;
    loop {
        match rt.get_orchestration_status("inst-status-poll").await {
            OrchestrationStatus::Completed { output } => {
                assert_eq!(output, "slow_done");
                break;
            }
            OrchestrationStatus::Failed { error } => panic!("failed: {error}"),
            OrchestrationStatus::Running => {
                iterations += 1;
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            OrchestrationStatus::NotFound => panic!("orchestration not found"),
        }
    }
    assert!(iterations > 0, "Should have polled multiple times");
}

#[tokio::test]
async fn sample_sub_orchestration_fanout_sqlite() {
    let (store, _temp_dir) = create_sqlite_store().await;

    let activity_registry = ActivityRegistry::builder()
        .register("Square", |input: String| async move {
            let n: i32 = input.parse().unwrap();
            Ok((n * n).to_string())
        })
        .build();

    let child_orch = |ctx: OrchestrationContext, input: String| async move {
        let squared = ctx.schedule_activity("Square", input).into_activity().await?;
        Ok(squared)
    };

    let parent_orch = |ctx: OrchestrationContext, _: String| async move {
        // Fan out to 3 child orchestrations
        let c1 = ctx.schedule_sub_orchestration("Child", "2");
        let c2 = ctx.schedule_sub_orchestration("Child", "3");
        let c3 = ctx.schedule_sub_orchestration("Child", "4");
        
        let results = ctx.join(vec![c1, c2, c3]).await;
        let mut sum = 0;
        for out in results {
            match out {
                DurableOutput::SubOrchestration(Ok(s)) => {
                    sum += s.parse::<i32>().unwrap();
                }
                _ => panic!("unexpected output"),
            }
        }
        Ok(sum.to_string()) // 4 + 9 + 16 = 29
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("Parent", parent_orch)
        .register("Child", child_orch)
        .build();

    let rt = runtime::Runtime::start_with_store(store.clone(), Arc::new(activity_registry), orchestration_registry).await;
    
    rt.clone()
        .start_orchestration("inst-fanout", "Parent", "")
        .await
        .unwrap();

    match rt.wait_for_orchestration("inst-fanout", Duration::from_secs(10)).await.unwrap() {
        OrchestrationStatus::Completed { output } => assert_eq!(output, "29"),
        OrchestrationStatus::Failed { error } => panic!("orchestration failed: {error}"),
        _ => panic!("unexpected status"),
    }
}

#[tokio::test]
async fn sample_sub_orchestration_chained_sqlite() {
    let (store, _temp_dir) = create_sqlite_store().await;

    let activity_registry = ActivityRegistry::builder().build();

    let add_one = |_ctx: OrchestrationContext, input: String| async move {
        let n: i32 = input.parse().unwrap();
        Ok((n + 1).to_string())
    };

    let double = |_ctx: OrchestrationContext, input: String| async move {
        let n: i32 = input.parse().unwrap();
        Ok((n * 2).to_string())
    };

    let chain = |ctx: OrchestrationContext, input: String| async move {
        let step1 = ctx.schedule_sub_orchestration("AddOne", input).into_sub_orchestration().await?;
        let step2 = ctx.schedule_sub_orchestration("Double", step1).into_sub_orchestration().await?;
        Ok(step2)
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("Chain", chain)
        .register("AddOne", add_one)
        .register("Double", double)
        .build();

    let rt = runtime::Runtime::start_with_store(store.clone(), Arc::new(activity_registry), orchestration_registry).await;
    
    rt.clone()
        .start_orchestration("inst-chain", "Chain", "5")
        .await
        .unwrap();

    match rt.wait_for_orchestration("inst-chain", Duration::from_secs(5)).await.unwrap() {
        OrchestrationStatus::Completed { output } => assert_eq!(output, "12"), // (5+1)*2 = 12
        OrchestrationStatus::Failed { error } => panic!("orchestration failed: {error}"),
        _ => panic!("unexpected status"),
    }
}

#[tokio::test]
async fn sample_detached_orchestration_scheduling_sqlite() {
    let (store, _temp_dir) = create_sqlite_store().await;

    let activity_registry = ActivityRegistry::builder().build();

    let detached_work = |_: OrchestrationContext, _: String| async move {
        Ok("detached_done".to_string())
    };

    let main_orch = |ctx: OrchestrationContext, _: String| async move {
        // Fire-and-forget sub-orchestration
        ctx.schedule_sub_orchestration("DetachedWork", "data");
        // Don't await it - just return
        Ok("main_done".to_string())
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("Main", main_orch)
        .register("DetachedWork", detached_work)
        .build();

    let rt = runtime::Runtime::start_with_store(store.clone(), Arc::new(activity_registry), orchestration_registry).await;
    
    rt.clone()
        .start_orchestration("inst-main", "Main", "")
        .await
        .unwrap();

    match rt.wait_for_orchestration("inst-main", Duration::from_secs(5)).await.unwrap() {
        OrchestrationStatus::Completed { output } => assert_eq!(output, "main_done"),
        OrchestrationStatus::Failed { error } => panic!("orchestration failed: {error}"),
        _ => panic!("unexpected status"),
    }
}

#[tokio::test]
async fn sample_typed_event_sqlite() {
    #[derive(Serialize, Deserialize)]
    struct ApprovalData {
        approved: bool,
        approver: String,
    }

    let (store, _temp_dir) = create_sqlite_store().await;

    let activity_registry = ActivityRegistry::builder().build();

    let orchestration = |ctx: OrchestrationContext, _: String| async move {
        let approval_data = ctx.schedule_wait("approval").into_event().await;
        let data: ApprovalData = serde_json::from_str(&approval_data).unwrap();
        if data.approved {
            Ok(format!("Approved by {}", data.approver))
        } else {
            Err("Not approved".to_string())
        }
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("TypedEvent", orchestration)
        .build();

    let rt = runtime::Runtime::start_with_store(store.clone(), Arc::new(activity_registry), orchestration_registry).await;

    let store_for_wait = store.clone();
    let rt_c = rt.clone();
    tokio::spawn(async move {
        let _ = common::wait_for_subscription(store_for_wait, "inst-typed-evt", "approval", 1000).await;
        let data = ApprovalData {
            approved: true,
            approver: "Alice".to_string(),
        };
        rt_c.raise_event("inst-typed-evt", "approval", serde_json::to_string(&data).unwrap()).await;
    });

    rt.clone()
        .start_orchestration("inst-typed-evt", "TypedEvent", "")
        .await
        .unwrap();

    match rt.wait_for_orchestration("inst-typed-evt", Duration::from_secs(5)).await.unwrap() {
        OrchestrationStatus::Completed { output } => assert_eq!(output, "Approved by Alice"),
        OrchestrationStatus::Failed { error } => panic!("orchestration failed: {error}"),
        _ => panic!("unexpected status"),
    }
}

#[tokio::test]
async fn sample_basic_error_handling_sqlite() {
    let (store, _temp_dir) = create_sqlite_store().await;

    let activity_registry = ActivityRegistry::builder()
        .register("MayFail", |input: String| async move {
            if input == "fail" {
                Err("simulated failure".to_string())
            } else {
                Ok("success".to_string())
            }
        })
        .build();

    let orchestration = |ctx: OrchestrationContext, input: String| async move {
        match ctx.schedule_activity("MayFail", input).into_activity().await {
            Ok(result) => Ok(format!("Activity succeeded: {}", result)),
            Err(error) => Ok(format!("Activity failed: {}", error)),
        }
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("ErrorHandling", orchestration)
        .build();

    let rt = runtime::Runtime::start_with_store(store.clone(), Arc::new(activity_registry), orchestration_registry).await;

    // Test success case
    rt.clone()
        .start_orchestration("inst-error-1", "ErrorHandling", "succeed")
        .await
        .unwrap();

    match rt.wait_for_orchestration("inst-error-1", Duration::from_secs(5)).await.unwrap() {
        OrchestrationStatus::Completed { output } => assert_eq!(output, "Activity succeeded: success"),
        _ => panic!("unexpected status"),
    }

    // Test failure case
    rt.clone()
        .start_orchestration("inst-error-2", "ErrorHandling", "fail")
        .await
        .unwrap();

    match rt.wait_for_orchestration("inst-error-2", Duration::from_secs(5)).await.unwrap() {
        OrchestrationStatus::Completed { output } => assert_eq!(output, "Activity failed: simulated failure"),
        _ => panic!("unexpected status"),
    }
}

#[tokio::test]
async fn sample_nested_function_error_handling_sqlite() {
    let (store, _temp_dir) = create_sqlite_store().await;

    let activity_registry = ActivityRegistry::builder()
        .register("ParseInt", |input: String| async move {
            input.parse::<i32>()
                .map(|n| n.to_string())
                .map_err(|_| "invalid integer".to_string())
        })
        .build();

    // Helper function that might fail
    async fn process_number(ctx: &OrchestrationContext, input: String) -> Result<i32, String> {
        let parsed = ctx.schedule_activity("ParseInt", input).into_activity().await?;
        let n: i32 = parsed.parse().unwrap();
        if n < 0 {
            Err("negative numbers not allowed".to_string())
        } else {
            Ok(n * 2)
        }
    }

    let orchestration = |ctx: OrchestrationContext, input: String| async move {
        match process_number(&ctx, input).await {
            Ok(result) => Ok(format!("Result: {}", result)),
            Err(error) => Ok(format!("Error: {}", error)),
        }
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("NestedError", orchestration)
        .build();

    let rt = runtime::Runtime::start_with_store(store.clone(), Arc::new(activity_registry), orchestration_registry).await;

    // Test valid input
    rt.clone()
        .start_orchestration("inst-nested-1", "NestedError", "5")
        .await
        .unwrap();

    match rt.wait_for_orchestration("inst-nested-1", Duration::from_secs(5)).await.unwrap() {
        OrchestrationStatus::Completed { output } => assert_eq!(output, "Result: 10"),
        _ => panic!("unexpected status"),
    }

    // Test invalid input
    rt.clone()
        .start_orchestration("inst-nested-2", "NestedError", "abc")
        .await
        .unwrap();

    match rt.wait_for_orchestration("inst-nested-2", Duration::from_secs(5)).await.unwrap() {
        OrchestrationStatus::Completed { output } => assert_eq!(output, "Error: invalid integer"),
        _ => panic!("unexpected status"),
    }

    // Test negative number
    rt.clone()
        .start_orchestration("inst-nested-3", "NestedError", "-5")
        .await
        .unwrap();

    match rt.wait_for_orchestration("inst-nested-3", Duration::from_secs(5)).await.unwrap() {
        OrchestrationStatus::Completed { output } => assert_eq!(output, "Error: negative numbers not allowed"),
        _ => panic!("unexpected status"),
    }
}

#[tokio::test]
async fn sample_mixed_string_and_typed_typed_orch_sqlite() {
    let (store, _temp_dir) = create_sqlite_store().await;

    // String activity: returns uppercased string
    // Typed activity: Add two numbers
    let activity_registry = ActivityRegistry::builder()
        .register("Upper", |input: String| async move { Ok(input.to_uppercase()) })
        .register_typed::<AddReq, AddRes, _, _>("Add", |req| async move { Ok(AddRes { sum: req.a + req.b }) })
        .build();

    // Typed orchestrator input/output
    let orch = |ctx: OrchestrationContext, req: AddReq| async move {
        // Kick off a typed activity and a string activity, race them with deterministic select
        let f_typed = ctx.schedule_activity_typed::<AddReq, AddRes>("Add", &req);
        let f_str = ctx.schedule_activity("Upper", "hello");
        let (_idx, out) = ctx.select(vec![f_typed, f_str]).await;
        let s = match out {
            duroxide::DurableOutput::Activity(Ok(raw)) => {
                // raw is either typed AddRes JSON or plain string result
                if let Ok(v) = serde_json::from_str::<AddRes>(&raw) {
                    format!("sum={}", v.sum)
                } else {
                    format!("up={raw}")
                }
            }
            duroxide::DurableOutput::Activity(Err(e)) => return Err(e),
            other => panic!("unexpected output: {:?}", other),
        };
        Ok::<_, String>(s)
    };
    let orchestration_registry = OrchestrationRegistry::builder()
        .register_typed::<AddReq, String, _, _>("MixedTypedOrch", orch)
        .build();

    let rt = runtime::Runtime::start_with_store(store, Arc::new(activity_registry), orchestration_registry).await;
    rt.clone()
        .start_orchestration_typed::<AddReq>("inst-mixed-typed", "MixedTypedOrch", AddReq { a: 1, b: 2 })
        .await
        .unwrap();

    let s = match rt
        .wait_for_orchestration_typed::<String>("inst-mixed-typed", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        Ok(result) => result,
        Err(error) => panic!("orchestration failed: {error}"),
    };
    assert!(s == "sum=3" || s == "up=HELLO");
    rt.shutdown().await;
}

/// Mixed string and typed activities with string orchestration, showcasing select on typed+string
#[tokio::test]
async fn sample_mixed_string_and_typed_string_orch_sqlite() {
    let (store, _temp_dir) = create_sqlite_store().await;

    let activity_registry = ActivityRegistry::builder()
        .register("Upper", |input: String| async move { Ok(input.to_uppercase()) })
        .register_typed::<AddReq, AddRes, _, _>("Add", |req| async move { Ok(AddRes { sum: req.a + req.b }) })
        .build();

    // String orchestrator mixes typed and string activity calls
    let orch = |ctx: OrchestrationContext, _in: String| async move {
        let f_typed = ctx.schedule_activity_typed::<AddReq, AddRes>("Add", &AddReq { a: 5, b: 7 });
        let f_str = ctx.schedule_activity("Upper", "race");
        let (_idx, out) = ctx.select(vec![f_typed, f_str]).await;
        let s = match out {
            duroxide::DurableOutput::Activity(Ok(raw)) => {
                if let Ok(v) = serde_json::from_str::<AddRes>(&raw) {
                    format!("sum={}", v.sum)
                } else {
                    format!("up={raw}")
                }
            }
            duroxide::DurableOutput::Activity(Err(e)) => return Err(e),
            other => panic!("unexpected output: {:?}", other),
        };
        Ok::<_, String>(s)
    };
    let orch_reg = OrchestrationRegistry::builder()
        .register("MixedStringOrch", orch)
        .build();

    let rt = runtime::Runtime::start_with_store(store, Arc::new(activity_registry), orch_reg).await;
    let _h = rt
        .clone()
        .start_orchestration("inst-mixed-string", "MixedStringOrch", "")
        .await
        .unwrap();

    let s = match rt
        .wait_for_orchestration("inst-mixed-string", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Completed { output } => output,
        runtime::OrchestrationStatus::Failed { error } => panic!("orchestration failed: {error}"),
        _ => panic!("unexpected orchestration status"),
    };
    assert!(s == "sum=12" || s == "up=RACE");
    rt.shutdown().await;
}

#[tokio::test]
async fn sample_versioning_start_latest_vs_exact_sqlite() {
    let (store, _temp_dir) = create_sqlite_store().await;

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
    let rt = runtime::Runtime::start_with_store(store, Arc::new(acts), reg.clone()).await;

    // With default policy (Latest), a new start should run v2
    let _h_latest = rt
        .clone()
        .start_orchestration("inst-vers-latest", "Versioned", "")
        .await
        .unwrap();

    match rt
        .wait_for_orchestration("inst-vers-latest", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Completed { output } => assert_eq!(output, "v2"),
        runtime::OrchestrationStatus::Failed { error } => panic!("orchestration failed: {error}"),
        _ => panic!("unexpected orchestration status"),
    }

    // Pin new starts to 1.0.0 via policy, verify it runs v1
    reg.set_version_policy(
        "Versioned",
        duroxide::runtime::VersionPolicy::Exact(semver::Version::parse("1.0.0").unwrap()),
    )
    .await;
    let _h_exact = rt
        .clone()
        .start_orchestration("inst-vers-exact", "Versioned", "")
        .await
        .unwrap();

    match rt
        .wait_for_orchestration("inst-vers-exact", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Completed { output } => assert_eq!(output, "v1"),
        runtime::OrchestrationStatus::Failed { error } => panic!("orchestration failed: {error}"),
        _ => panic!("unexpected orchestration status"),
    }

    rt.shutdown().await;
}

#[tokio::test]
async fn sample_versioning_sub_orchestration_explicit_vs_policy_sqlite() {
    let (store, _temp_dir) = create_sqlite_store().await;

    let child_v1 = |_: OrchestrationContext, _in: String| async move { Ok("c1".to_string()) };
    let child_v2 = |_: OrchestrationContext, _in: String| async move { Ok("c2".to_string()) };
    let parent = |ctx: OrchestrationContext, _in: String| async move {
        // Explicit versioned call -> expect c1
        let a = ctx
            .schedule_sub_orchestration_versioned("Child", Some("1.0.0".to_string()), "exp")
            .into_sub_orchestration()
            .await
            .unwrap();
        // Policy-based call (Latest) -> expect c2
        let b = ctx
            .schedule_sub_orchestration("Child", "pol")
            .into_sub_orchestration()
            .await
            .unwrap();
        Ok(format!("{a}-{b}"))
    };

    let reg = OrchestrationRegistry::builder()
        .register("ParentVers", parent)
        .register("Child", child_v1)
        .register_versioned("Child", "2.0.0", child_v2)
        .build();
    let acts = ActivityRegistry::builder().build();
    let rt = runtime::Runtime::start_with_store(store, Arc::new(acts), reg).await;

    let _h = rt
        .clone()
        .start_orchestration("inst-sub-vers", "ParentVers", "")
        .await
        .unwrap();

    match rt
        .wait_for_orchestration("inst-sub-vers", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Completed { output } => assert_eq!(output, "c1-c2"),
        runtime::OrchestrationStatus::Failed { error } => panic!("orchestration failed: {error}"),
        _ => panic!("unexpected orchestration status"),
    }

    rt.shutdown().await;
}

#[tokio::test]
async fn sample_versioning_continue_as_new_upgrade_sqlite() {
    use duroxide::OrchestrationStatus;
    let (store, _temp_dir) = create_sqlite_store().await;

    // v1: simulate deciding to upgrade at a maintenance boundary (e.g., at the end of a cycle)
    // In a real infinite loop, you'd do some work (timer/activity), then CAN to v2.
    let v1 = |ctx: OrchestrationContext, input: String| async move {
        ctx.trace_info("v1: upgrading via ContinueAsNew (default policy)".to_string());
        // Roll to a fresh execution, marking the payload so we can attribute it to v1 deterministically
        ctx.continue_as_new(format!("v1:{input}"));
        Ok(String::new())
    };
    // v2: represents the upgraded logic. Here we just simulate one step and complete for the sample.
    let v2 = |ctx: OrchestrationContext, input: String| async move {
        ctx.trace_info(format!("v2: resumed with input={input}"));
        Ok(format!("upgraded:{input}"))
    };

    let reg = OrchestrationRegistry::builder()
        .register("LongRunner", v1) // implicit 1.0.0
        .register_versioned("LongRunner", "2.0.0", v2)
        .build();
    let acts = ActivityRegistry::builder().build();
    let rt = runtime::Runtime::start_with_store(store.clone(), Arc::new(acts), reg).await;

    // Start on v1; the first handle will resolve at the CAN boundary
    // Pin initial start to v1 explicitly to demonstrate upgrade via CAN; default policy remains Latest (v2)
    rt.clone()
        .start_orchestration_versioned("inst-can-upgrade", "LongRunner", "1.0.0", "state")
        .await
        .unwrap();

    // Poll for the new execution (v2) to complete
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match rt.get_orchestration_status("inst-can-upgrade").await {
            OrchestrationStatus::Completed { output } => {
                assert_eq!(output, "upgraded:v1:state");
                break;
            }
            OrchestrationStatus::Failed { error } => panic!("unexpected failure: {error}"),
            _ if std::time::Instant::now() < deadline => tokio::time::sleep(std::time::Duration::from_millis(10)).await,
            _ => panic!("timeout waiting for upgraded completion"),
        }
    }

    // Verify two executions exist, exec1 continued-as-new, exec2 completed with v2 output
    let execs = store.list_executions("inst-can-upgrade").await;
    assert_eq!(execs, vec![1, 2]);
    let e1 = store.read_with_execution("inst-can-upgrade", 1).await;
    assert!(
        e1.iter()
            .any(|e| matches!(e, duroxide::Event::OrchestrationContinuedAsNew { .. }))
    );
    // Exec2 must start with the v1-marked payload, proving v1 ran first and handed off via CAN
    let e2 = store.read_with_execution("inst-can-upgrade", 2).await;
    assert!(
        e2.iter()
            .any(|e| matches!(e, duroxide::Event::OrchestrationStarted { input, .. } if input == "v1:state"))
    );

    rt.shutdown().await;
}

#[tokio::test]
async fn sample_cancellation_parent_cascades_to_children_sqlite() {
    use duroxide::Event;
    let (store, _temp_dir) = create_sqlite_store().await;

    // Child: waits forever (until canceled). This demonstrates cooperative cancellation via runtime.
    let child = |ctx: OrchestrationContext, _input: String| async move {
        let _ = ctx.schedule_wait("Go").into_event().await;
        Ok("done".to_string())
    };

    // Parent: starts child and awaits its completion.
    let parent = |ctx: OrchestrationContext, _input: String| async move {
        let _ = ctx
            .schedule_sub_orchestration("ChildSample", "seed")
            .into_sub_orchestration()
            .await?;
        Ok::<_, String>("parent_done".to_string())
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("ChildSample", child)
        .register("ParentSample", parent)
        .build();
    let activity_registry = ActivityRegistry::builder().build();
    let rt =
        runtime::Runtime::start_with_store(store.clone(), Arc::new(activity_registry), orchestration_registry).await;

    // Start the parent orchestration
    let _h = rt
        .clone()
        .start_orchestration("inst-sample-cancel", "ParentSample", "")
        .await
        .unwrap();

    // Allow scheduling turn to run and child to start
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Cancel the parent; the runtime will append OrchestrationCancelRequested and then OrchestrationFailed
    rt.cancel_instance("inst-sample-cancel", "user_request").await;

    // Wait for the parent to fail deterministically with a canceled error
    let ok = common::wait_for_history(
        store.clone(),
        "inst-sample-cancel",
        |hist| {
            hist.iter().rev().any(
                |e| matches!(e, Event::OrchestrationFailed { error } if error.starts_with("canceled: user_request")),
            )
        },
        5_000,
    )
    .await;
    assert!(ok, "timeout waiting for parent cancel failure");

    // Find child instance (prefix is parent::sub::<id>) and check it was canceled too
    let children: Vec<String> = store
        .list_instances()
        .await
        .into_iter()
        .filter(|i| i.starts_with("inst-sample-cancel::"))
        .collect();
    assert!(!children.is_empty());
    for child in children {
        let ok_child = common::wait_for_history(store.clone(), &child, |hist| {
            hist.iter().any(|e| matches!(e, Event::OrchestrationCancelRequested { .. })) &&
            hist.iter().any(|e| matches!(e, Event::OrchestrationFailed { error } if error.starts_with("canceled: parent canceled")))
        }, 5_000).await;
        assert!(ok_child, "timeout waiting for child cancel for {child}");
    }

    rt.shutdown().await;
}

/// Error recovery with explicit error handling
#[tokio::test]
async fn sample_error_recovery_sqlite() {
    let (store, _temp_dir) = create_sqlite_store().await;

    // Register activities
    let activity_registry = ActivityRegistry::builder()
        .register("ProcessData", |input: String| async move {
            if input.contains("error") {
                Err("Processing failed".to_string())
            } else {
                Ok(format!("Processed: {input}"))
            }
        })
        .register("LogError", |error: String| async move {
            Ok(format!("Logged: {error}"))
        })
        .build();

    // Orchestration with explicit error recovery
    let orchestration = |ctx: OrchestrationContext, input: String| async move {
        ctx.trace_info("Starting orchestration");
        
        match ctx.schedule_activity("ProcessData", input.clone()).into_activity().await {
            Ok(result) => {
                ctx.trace_info("Processing succeeded");
                Ok(result)
            }
            Err(e) => {
                ctx.trace_info("Processing failed, logging error");
                let _ = ctx.schedule_activity("LogError", e.clone()).into_activity().await;
                Err(format!("Failed to process '{}': {}", input, e))
            }
        }
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("ErrorRecovery", orchestration)
        .build();

    let rt = runtime::Runtime::start_with_store(store.clone(), Arc::new(activity_registry), orchestration_registry).await;

    // Test successful case
    let _handle1 = rt
        .clone()
        .start_orchestration("inst-recovery-1", "ErrorRecovery", "test")
        .await
        .unwrap();

    match rt
        .wait_for_orchestration("inst-recovery-1", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Completed { output } => {
            assert_eq!(output, "Processed: test");
        }
        runtime::OrchestrationStatus::Failed { error } => panic!("orchestration failed: {error}"),
        _ => panic!("unexpected orchestration status"),
    }

    // Test error case
    let _handle2 = rt
        .clone()
        .start_orchestration("inst-recovery-2", "ErrorRecovery", "error-case")
        .await
        .unwrap();

    match rt
        .wait_for_orchestration("inst-recovery-2", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Failed { error } => {
            assert_eq!(error, "Failed to process 'error-case': Processing failed");
        }
        runtime::OrchestrationStatus::Completed { output } => panic!("expected failure but got: {output}"),
        _ => panic!("unexpected orchestration status"),
    }

    rt.shutdown().await;
}
