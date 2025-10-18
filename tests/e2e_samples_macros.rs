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
        let res = ctx.schedule_activity("hello", "Rust").into_activity().await?;
        ctx.trace_info(format!("hello_world result={res} "));
        let res1 = ctx.schedule_activity("hello", input).into_activity().await?;
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

/// Simple orchestration using macros
///
/// Highlights:
/// - Basic orchestration with macros
/// - Auto-discovery
#[tokio::test]
async fn sample_simple_orchestration_macros() {
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;

    // Define orchestration using macro
    #[orchestration]
    async fn simple_orchestration(ctx: OrchestrationContext, input: String) -> Result<String, String> {
        ctx.trace_info("Starting simple orchestration");
        ctx.trace_info(format!("Input: {}", input));
        Ok(format!("Processed: {}", input))
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
        .start_orchestration("inst-simple-1", "simple_orchestration", "TestInput")
        .await
        .unwrap();

    match client
        .wait_for_orchestration("inst-simple-1", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        duroxide::OrchestrationStatus::Completed { output } => {
            assert_eq!(output, "Processed: TestInput");
        }
        _ => panic!("Expected completion"),
    }

    rt.shutdown().await;
}