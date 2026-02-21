//! PoC: Three request-handling patterns in one orchestration.
//!
//! Uses schedule_wait + raise_event as a stand-in for request/response.
//! Validates that all three patterns compile and run correctly with
//! the duroxide runtime (replay, multi-turn, etc).
#![allow(clippy::unwrap_used)]
#![allow(clippy::clone_on_ref_ptr)]
#![allow(clippy::expect_used)]

use duroxide::runtime::registry::ActivityRegistry;
use duroxide::runtime::{self};
use duroxide::{ActivityContext, Client, OrchestrationContext, OrchestrationRegistry, OrchestrationStatus};
use std::sync::{Arc, Mutex};
use std::time::Duration;
mod common;

#[derive(Debug, Clone)]
struct OrderState {
    status: String,
    items_done: u32,
}

/// Single orchestration exercising all three Rust-compatible handler patterns:
///
/// 1. **Inline** — handle the request right in the flow (schedule_wait + inline code).
///    Simplest, most idiomatic. Full access to all locals. Zero overhead.
///
/// 2. **async fn with params** — define a reusable local function, pass state each call.
///    No ownership issues but verbose parameter lists.
///
/// 3. **Arc<Mutex<T>> closure** — shared mutable state between mainline and closure.
///    Most flexible (both sides read/write), but double-clone boilerplate.
#[tokio::test]
async fn poc_all_three_patterns_in_one_orchestration() {
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;

    let activities = ActivityRegistry::builder()
        .register("Enrich", |_ctx: ActivityContext, input: String| async move {
            Ok(format!("enriched:{input}"))
        })
        .register("Process", |_ctx: ActivityContext, input: String| async move {
            Ok(format!("processed:{input}"))
        })
        .build();

    let orchestration = |ctx: OrchestrationContext, input: String| async move {
        let mut status = "starting".to_string();
        let mut items_done = 0u32;

        let _result = ctx.schedule_activity("Process", &input).await?;
        status = "phase1-done".to_string();
        items_done = 1;

        // ══════════════════════════════════════════════════════════════
        // Pattern 1: Inline — just handle it right here in the flow.
        //
        // Simplest, most idiomatic Rust. Full access to all locals.
        // This is what `receive_request().await` would look like.
        // ══════════════════════════════════════════════════════════════
        let req1_payload = ctx.schedule_wait("req:Inline").await;
        let r1 = {
            let snapshot = format!(
                r#"{{"pattern":"inline","status":"{status}","items":{items_done},"req":{req1_payload}}}"#
            );
            ctx.schedule_activity("Enrich", &snapshot).await?
        };
        // Still have full access — mutate freely
        status = "after-inline".to_string();
        items_done = 2;

        // ══════════════════════════════════════════════════════════════
        // Pattern 2: async fn — define handler as a local function,
        //            pass state as parameters each call.
        // ══════════════════════════════════════════════════════════════
        async fn handle_with_params(
            ctx: &OrchestrationContext,
            status: &str,
            items_done: u32,
            payload: &str,
        ) -> Result<String, String> {
            let snapshot = format!(
                r#"{{"pattern":"async_fn","status":"{status}","items":{items_done},"req":{payload}}}"#
            );
            ctx.schedule_activity("Enrich", &snapshot).await
        }

        let req2_payload = ctx.schedule_wait("req:AsyncFn").await;
        let r2 = handle_with_params(&ctx, &status, items_done, &req2_payload).await?;

        // Mutate between calls — handler sees fresh state next time
        status = "after-asyncfn".to_string();
        items_done = 3;

        let req3_payload = ctx.schedule_wait("req:AsyncFn2").await;
        let r3 = handle_with_params(&ctx, &status, items_done, &req3_payload).await?;

        // ══════════════════════════════════════════════════════════════
        // Pattern 3: Arc<Mutex<T>> closure — shared mutable state
        //            between main orchestration and reusable closure.
        // ══════════════════════════════════════════════════════════════
        let shared = Arc::new(Mutex::new(OrderState {
            status: status.clone(),
            items_done,
        }));

        let handle_with_arc = {
            let shared = shared.clone();
            let ctx = ctx.clone();
            move |payload: String| {
                let shared = shared.clone();
                let ctx = ctx.clone();
                async move {
                    let snapshot = {
                        let s = shared.lock().unwrap();
                        format!(
                            r#"{{"pattern":"arc_mutex","status":"{}","items":{},"req":{}}}"#,
                            s.status, s.items_done, payload
                        )
                    };
                    ctx.schedule_activity("Enrich", &snapshot).await
                }
            }
        };

        let req4_payload = ctx.schedule_wait("req:Arc1").await;
        let r4 = handle_with_arc(req4_payload).await?;

        // Both main and closure can mutate — main updates, closure sees it
        {
            let mut s = shared.lock().unwrap();
            s.status = "arc-updated".to_string();
            s.items_done = 99;
        }

        let req5_payload = ctx.schedule_wait("req:Arc2").await;
        let r5 = handle_with_arc(req5_payload).await?;

        // ══════════════════════════════════════════════════════════════
        // Verify all five responses
        // ══════════════════════════════════════════════════════════════
        assert!(r1.contains(r#""pattern":"inline""#), "r1: {r1}");
        assert!(r1.contains("phase1-done"), "r1 status: {r1}");

        assert!(r2.contains(r#""pattern":"async_fn""#), "r2: {r2}");
        assert!(r2.contains("after-inline"), "r2 status: {r2}");

        assert!(r3.contains(r#""pattern":"async_fn""#), "r3: {r3}");
        assert!(r3.contains("after-asyncfn"), "r3 status: {r3}");
        assert!(r3.contains(r#""items":3"#), "r3 items: {r3}");

        assert!(r4.contains(r#""pattern":"arc_mutex""#), "r4: {r4}");
        assert!(r4.contains("after-asyncfn"), "r4 status: {r4}");

        assert!(r5.contains(r#""pattern":"arc_mutex""#), "r5: {r5}");
        assert!(r5.contains("arc-updated"), "r5 status: {r5}");
        assert!(r5.contains(r#""items":99"#), "r5 items: {r5}");

        Ok(format!("{r1}|{r2}|{r3}|{r4}|{r5}"))
    };

    let orchestrations = OrchestrationRegistry::builder()
        .register("AllThree", orchestration)
        .build();

    let rt = runtime::Runtime::start_with_store(store.clone(), activities, orchestrations).await;
    let client = Client::new(store.clone());
    let inst = "poc-all-three";

    client.start_orchestration(inst, "AllThree", r#""order-1""#).await.unwrap();

    // Send 5 requests in sequence, waiting for each subscription
    for event_name in [
        "req:Inline",
        "req:AsyncFn",
        "req:AsyncFn2",
        "req:Arc1",
        "req:Arc2",
    ] {
        common::wait_for_subscription(store.clone(), inst, event_name, 3000).await;
        client.raise_event(inst, event_name, r#"{"v":1}"#).await.unwrap();
    }

    let result = client.wait_for_orchestration(inst, Duration::from_secs(10)).await.unwrap();

    match result {
        OrchestrationStatus::Completed { output, .. } => {
            println!("ALL THREE PATTERNS PASSED:\n{output}");
            assert_eq!(output.matches("enriched:").count(), 5, "should have 5 enriched responses");
        }
        other => panic!("Expected Completed, got: {other:?}"),
    }

    rt.shutdown(None).await;
}
