//! End-to-end samples with macros: start here to learn the macro API by example.
//!
//! Each test demonstrates a common orchestration pattern using
//! `OrchestrationContext` and the macro-based auto-discovery system.
use duroxide::runtime::{self};
use duroxide::{Client, OrchestrationContext};
use duroxide::macros::*;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
mod common;

/// Hello World: define one activity and call it from an orchestrator using macros.
///
/// Highlights:
/// - Use #[activity] attribute for automatic registration
/// - Use #[orchestration] attribute for automatic registration
/// - Use schedule!() macro for type-safe activity calls
/// - Auto-discovery with Runtime::builder()
#[tokio::test]
async fn sample_hello_world_macros() {
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;

    // Define activity using macro
    #[activity]
    async fn hello(input: String) -> Result<String, String> {
        Ok(format!("Hello, {input}!"))
    }

    // Define orchestration using macro
    #[orchestration]
    async fn hello_world(ctx: OrchestrationContext, input: String) -> Result<String, String> {
        ctx.trace_info("hello_world started");
        let res = schedule!(ctx, hello("Rust")).await?;
        ctx.trace_info(format!("hello_world result={res} "));
        let res1 = schedule!(ctx, hello(input)).await?;
        ctx.trace_info(format!("hello_world result={res1} "));
        Ok(res1)
    }

    // Auto-discovery!
    let rt = runtime::Runtime::builder()
        .store(store.clone())
        .discover_activities()
        .discover_orchestrations()
        .start()
        .await
        .unwrap();

    let client = Client::new(store.clone());
    client
        .start_orchestration("inst-sample-hello-1", "hello_world", "World")
        .await
        .unwrap();

    match client
        .wait_for_orchestration("inst-sample-hello-1", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        duroxide::OrchestrationStatus::Completed { output } => {
            assert_eq!(output, "Hello, World!");
        }
        _ => panic!("Expected completion"),
    }

    rt.shutdown().await;
}

/// Fan-out/Fan-in pattern using macros
///
/// Highlights:
/// - Parallel execution of multiple activities
/// - Deterministic result aggregation with ctx.join()
/// - Type-safe activity calls with schedule!() macro
#[tokio::test]
async fn sample_fan_out_fan_in_macros() {
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;

    #[derive(Clone, Serialize, Deserialize, Debug)]
    struct Task {
        id: u32,
        name: String,
    }

    #[derive(Clone, Serialize, Deserialize, Debug)]
    struct TaskResult {
        task_id: u32,
        result: String,
    }

    // Define activities using macros
    #[activity]
    async fn process_task(task: Task) -> Result<TaskResult, String> {
        // Simulate work
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        Ok(TaskResult {
            task_id: task.id,
            result: format!("Processed {}", task.name),
        })
    }

    // Define orchestration using macro
    #[orchestration]
    async fn fan_out_fan_in(ctx: OrchestrationContext, tasks: Vec<Task>) -> Result<String, String> {
        ctx.trace_info(format!("Starting fan-out/fan-in for {} tasks", tasks.len()));

        // Fan-out: Schedule all tasks in parallel
        let futures: Vec<_> = tasks
            .into_iter()
            .map(|task| schedule!(ctx, process_task(task)))
            .collect();

        // Fan-in: Wait for all to complete deterministically
        let results = ctx.join(futures).await;

        // Count successful results
        let mut success_count = 0;
        for result in results {
            match result {
                duroxide::DurableOutput::Activity(Ok(_)) => success_count += 1,
                duroxide::DurableOutput::Activity(Err(e)) => return Err(e),
                _ => unreachable!(),
            }
        }

        Ok(format!("Processed {} tasks successfully", success_count))
    }

    // Auto-discovery!
    let rt = runtime::Runtime::builder()
        .store(store.clone())
        .discover_activities()
        .discover_orchestrations()
        .start()
        .await
        .unwrap();

    let client = Client::new(store.clone());

    // Create test tasks
    let tasks = vec![
        Task { id: 1, name: "Task 1".to_string() },
        Task { id: 2, name: "Task 2".to_string() },
        Task { id: 3, name: "Task 3".to_string() },
    ];

    client
        .start_orchestration("inst-fan-out-1", "fan_out_fan_in", tasks)
        .await
        .unwrap();

    match client
        .wait_for_orchestration("inst-fan-out-1", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        duroxide::OrchestrationStatus::Completed { output } => {
            assert_eq!(output, "Processed 3 tasks successfully");
        }
        _ => panic!("Expected completion"),
    }

    rt.shutdown().await;
}

/// Timer and external event pattern using macros
///
/// Highlights:
/// - Using timers for timeouts
/// - Waiting for external events
/// - Deterministic selection with ctx.select2()
#[tokio::test]
async fn sample_timer_and_events_macros() {
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;

    // Define orchestration using macro
    #[orchestration]
    async fn timer_and_events(ctx: OrchestrationContext, timeout_ms: u64) -> Result<String, String> {
        ctx.trace_info("Starting timer and events orchestration");

        // Schedule a timer and wait for external event
        let timer = ctx.schedule_timer(timeout_ms);
        let event_wait = ctx.schedule_wait("TestEvent");

        // Use deterministic select2 to wait for either timer or event
        let (winner, _) = ctx.select2(timer, event_wait).await;

        match winner {
            0 => {
                ctx.trace_info("Timer fired first");
                Ok("Timeout".to_string())
            }
            1 => {
                ctx.trace_info("Event received first");
                let event_data = ctx.schedule_wait("TestEvent").into_event().await;
                Ok(format!("Event: {}", event_data))
            }
            _ => unreachable!(),
        }
    }

    // Auto-discovery!
    let rt = runtime::Runtime::builder()
        .store(store.clone())
        .discover_activities()
        .discover_orchestrations()
        .start()
        .await
        .unwrap();

    let client = Client::new(store.clone());

    // Start orchestration with 100ms timeout
    client
        .start_orchestration("inst-timer-1", "timer_and_events", 100u64)
        .await
        .unwrap();

    // Send event after 50ms (should arrive before timeout)
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    client
        .raise_event("inst-timer-1", "TestEvent", "TestData")
        .await
        .unwrap();

    match client
        .wait_for_orchestration("inst-timer-1", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        duroxide::OrchestrationStatus::Completed { output } => {
            assert_eq!(output, "Event: TestData");
        }
        _ => panic!("Expected completion"),
    }

    rt.shutdown().await;
}

/// Sub-orchestration pattern using macros
///
/// Highlights:
/// - Calling sub-orchestrations
/// - Hierarchical orchestration structure
/// - Type-safe sub-orchestration calls
#[tokio::test]
async fn sample_sub_orchestration_macros() {
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;

    // Define sub-orchestration
    #[orchestration]
    async fn sub_task(ctx: OrchestrationContext, input: String) -> Result<String, String> {
        ctx.trace_info(format!("Sub-task processing: {}", input));
        Ok(format!("Sub-result: {}", input))
    }

    // Define main orchestration
    #[orchestration]
    async fn main_orchestration(ctx: OrchestrationContext, input: String) -> Result<String, String> {
        ctx.trace_info("Main orchestration started");

        // Call sub-orchestration using macro
        let sub_result = schedule!(ctx, sub_task(input)).await?;

        ctx.trace_info(format!("Sub-orchestration completed: {}", sub_result));
        Ok(format!("Main result: {}", sub_result))
    }

    // Auto-discovery!
    let rt = runtime::Runtime::builder()
        .store(store.clone())
        .discover_activities()
        .discover_orchestrations()
        .start()
        .await
        .unwrap();

    let client = Client::new(store.clone());

    client
        .start_orchestration("inst-main-1", "main_orchestration", "TestInput")
        .await
        .unwrap();

    match client
        .wait_for_orchestration("inst-main-1", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        duroxide::OrchestrationStatus::Completed { output } => {
            assert_eq!(output, "Main result: Sub-result: TestInput");
        }
        _ => panic!("Expected completion"),
    }

    rt.shutdown().await;
}

/// Durable system calls using macros
///
/// Highlights:
/// - Deterministic GUID generation
/// - Deterministic timestamp generation
/// - Replay-safe logging
#[tokio::test]
async fn sample_durable_system_calls_macros() {
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;

    // Define orchestration using macro
    #[orchestration]
    async fn durable_system_calls(ctx: OrchestrationContext, _input: String) -> Result<String, String> {
        ctx.trace_info("Starting durable system calls test");

        // Generate deterministic GUID
        let guid = durable_newguid!().await?;
        ctx.trace_info(format!("Generated GUID: {}", guid));

        // Get deterministic timestamp
        let timestamp = durable_utcnow!().await?;
        ctx.trace_info(format!("Current timestamp: {}", timestamp));

        // Use durable tracing
        durable_trace_info!("This is a durable trace message");
        durable_trace_warn!("This is a durable warning message");

        Ok(format!("GUID: {}, Timestamp: {}", guid, timestamp))
    }

    // Auto-discovery!
    let rt = runtime::Runtime::builder()
        .store(store.clone())
        .discover_activities()
        .discover_orchestrations()
        .start()
        .await
        .unwrap();

    let client = Client::new(store.clone());

    client
        .start_orchestration("inst-durable-1", "durable_system_calls", "TestInput")
        .await
        .unwrap();

    match client
        .wait_for_orchestration("inst-durable-1", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        duroxide::OrchestrationStatus::Completed { output } => {
            assert!(output.contains("GUID:"));
            assert!(output.contains("Timestamp:"));
        }
        _ => panic!("Expected completion"),
    }

    rt.shutdown().await;
}
