// Test: Orchestration code swap mid-execution triggers nondeterminism
// This test starts an orchestration with code A, then swaps the registry to code B before completion.
// It expects a nondeterminism error when a completion arrives for an activity/timer not scheduled by the new code.

use std::sync::Arc as StdArc;
use rust_dtf::{Event, OrchestrationContext, OrchestrationRegistry, OrchestrationStatus};
use rust_dtf::runtime::{self};
use rust_dtf::runtime::registry::ActivityRegistry;
use rust_dtf::providers::HistoryStore;
use rust_dtf::providers::fs::FsHistoryStore;
mod common;

#[tokio::test]
async fn code_swap_triggers_nondeterminism() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path(), true)) as StdArc<dyn HistoryStore>;

    // Register both A1 and B1 activities at all times
    let activity_registry = ActivityRegistry::builder()
        // A1 never completes (simulate long-running or blocked work)
        .register("A1", |_input: String| async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            }
            #[allow(unreachable_code)]
            Ok(String::new())
        })
        // B1 completes quickly
        .register("B1", |input: String| async move { Ok(format!("B1:{{{input}}}")) })
        .build();

    // Code A: schedules activity "A1" then waits for completion
    let orch_a = |ctx: OrchestrationContext, _input: String| async move {
        let res = ctx.schedule_activity("A1", "foo").into_activity().await.unwrap();
        Ok(res)
    };
    // Code B: schedules activity "B1" (different name/id)
    let orch_b = |ctx: OrchestrationContext, _input: String| async move {
        let res = ctx.schedule_activity("B1", "bar").into_activity().await.unwrap();
        Ok(res)
    };

    // Register A, start orchestration
    let reg_a = OrchestrationRegistry::builder().register("SwapTest", orch_a).build();
    let rt_a = runtime::Runtime::start_with_store(store.clone(), StdArc::new(activity_registry.clone()), reg_a).await;

    let _h = rt_a.clone().start_orchestration("inst-swap", "SwapTest", "").await.unwrap();

    // Wait for ActivityScheduled("A1") to appear in history and capture it
    let evt = common::wait_for_history_event(store.clone(), "inst-swap", |hist| {
        hist.iter().find_map(|e| match e {
            Event::ActivityScheduled { name, .. } if name == "A1" => Some(e.clone()),
            _ => None,
        })
    }, 2000).await;
    match evt {
        Some(Event::ActivityScheduled { .. }) => {},
        _ => panic!("timed out waiting for A1 schedule"),
    }

    // Simulate code swap: drop old runtime, create new one with registry B
    drop(rt_a);
    let reg_b = OrchestrationRegistry::builder().register("SwapTest", orch_b).build();
    let rt_b = runtime::Runtime::start_with_store(store.clone(), StdArc::new(activity_registry), reg_b).await;

    // Wait for terminal status using helper
    match rt_b.wait_for_orchestration("inst-swap", std::time::Duration::from_secs(5)).await.unwrap() {
        OrchestrationStatus::Failed { error } => assert!(error.contains("nondeterministic"), "error: {error}"),
        other => panic!("expected failure with nondeterminism, got: {other:?}"),
    }
}
