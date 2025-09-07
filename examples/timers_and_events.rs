//! Timers and External Events Example
//!
//! This example demonstrates:
//! - Using durable timers for delays
//! - Waiting for external events (approvals, webhooks, etc.)
//! - Control flow with select2 for race conditions
//! - Human-in-the-loop workflows
//!
//! Run with: `cargo run --example timers_and_events`

use duroxide::providers::sqlite::SqliteHistoryStore;
use duroxide::runtime::registry::ActivityRegistry;
use duroxide::runtime::{self};
use duroxide::{OrchestrationContext, OrchestrationRegistry, DurableOutput, DuroxideClient};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

#[derive(Serialize, Deserialize, Debug)]
struct ApprovalRequest {
    request_id: String,
    amount: f64,
    requester: String,
}

#[derive(Serialize, Deserialize, Debug)]
struct ApprovalResponse {
    request_id: String,
    approved: bool,
    approver: String,
    comments: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let temp_dir = tempfile::tempdir()?;
    let db_path = temp_dir.path().join("timers_and_events.db");
    std::fs::File::create(&db_path)?;
    let db_url = format!("sqlite:{}", db_path.to_str().unwrap());
    let store = Arc::new(SqliteHistoryStore::new(&db_url).await?);

    // Register activities for the approval workflow
    let activities = ActivityRegistry::builder()
        .register("SubmitForApproval", |request_json: String| async move {
            let request: ApprovalRequest = serde_json::from_str(&request_json)
                .map_err(|e| format!("JSON parse error: {}", e))?;
            // Simulate submitting to an approval system
            tokio::time::sleep(Duration::from_millis(100)).await;
            println!("📋 Approval request submitted: {} for ${:.2}", request.request_id, request.amount);
            Ok(format!("Request {} submitted for approval", request.request_id))
        })
        .register("ProcessApproval", |response_json: String| async move {
            let response: ApprovalResponse = serde_json::from_str(&response_json)
                .map_err(|e| format!("JSON parse error: {}", e))?;
            // Simulate processing the approval
            tokio::time::sleep(Duration::from_millis(50)).await;
            let status = if response.approved { "APPROVED" } else { "REJECTED" };
            println!("✅ Approval processed: {} - {}", response.request_id, status);
            Ok(format!("Request {} {}", response.request_id, status))
        })
        .register("SendReminder", |request_id: String| async move {
            // Simulate sending a reminder
            tokio::time::sleep(Duration::from_millis(25)).await;
            println!("📧 Reminder sent for request: {}", request_id);
            Ok(format!("Reminder sent for {}", request_id))
        })
        .build();

    // Orchestration that demonstrates timers and external events
    let orchestration = |ctx: OrchestrationContext, request_json: String| async move {
        ctx.trace_info("Starting approval workflow orchestration");
        
        let request: ApprovalRequest = serde_json::from_str(&request_json)
            .map_err(|e| format!("JSON parse error: {}", e))?;
        ctx.trace_info(format!("Processing approval request: {}", request.request_id));

        // Submit the request for approval
        let request_json = serde_json::to_string(&request)
            .map_err(|e| format!("JSON serialize error: {}", e))?;
        ctx.schedule_activity("SubmitForApproval", request_json)
            .into_activity()
            .await?;

        // Set up a race between approval and timeout
        let approval_timeout = ctx.schedule_timer(5000); // 5 second timeout
        let approval_event = ctx.schedule_wait("ApprovalEvent");

        ctx.trace_info("Waiting for approval or timeout...");

        // Race between approval event and timeout
        let (winner_index, result) = ctx.select2(approval_timeout, approval_event).await;

        match (winner_index, result) {
            (0, DurableOutput::Timer) => {
                // Timeout occurred - send reminder and wait longer
                ctx.trace_warn("Approval timeout - sending reminder");
                ctx.schedule_activity("SendReminder", &request.request_id)
                    .into_activity()
                    .await?;

                // Wait a bit longer for approval
                let extended_timeout = ctx.schedule_timer(3000); // 3 more seconds
                let approval_event2 = ctx.schedule_wait("ApprovalEvent");

                let (_, result2) = ctx.select2(extended_timeout, approval_event2).await;
                match result2 {
                    DurableOutput::External(approval_json) => {
                        let response: ApprovalResponse = serde_json::from_str(&approval_json)
                            .map_err(|e| format!("JSON parse error: {}", e))?;
                        let response_json = serde_json::to_string(&response)
                            .map_err(|e| format!("JSON serialize error: {}", e))?;
                        ctx.schedule_activity("ProcessApproval", response_json)
                            .into_activity()
                            .await?;
                        Ok(format!("Request {} processed after reminder", request.request_id))
                    }
                    DurableOutput::Timer => {
                        ctx.trace_error("Final timeout - request expired");
                        Ok(format!("Request {} expired after timeout", request.request_id))
                    }
                    _ => Err("Unexpected result type".to_string()),
                }
            }
            (1, DurableOutput::External(approval_json)) => {
                // Approval received within timeout
                let response: ApprovalResponse = serde_json::from_str(&approval_json)
                    .map_err(|e| format!("JSON parse error: {}", e))?;
                let response_json = serde_json::to_string(&response)
                    .map_err(|e| format!("JSON serialize error: {}", e))?;
                ctx.schedule_activity("ProcessApproval", response_json)
                    .into_activity()
                    .await?;
                Ok(format!("Request {} processed promptly", request.request_id))
            }
            _ => Err("Unexpected race result".to_string()),
        }
    };

    let orchestrations = OrchestrationRegistry::builder()
        .register("ApprovalWorkflow", orchestration)
        .build();

    let rt = runtime::Runtime::start_with_store(
        store.clone(),
        Arc::new(activities),
        orchestrations,
    ).await;

    // Create a test approval request
    let request = ApprovalRequest {
        request_id: "REQ-001".to_string(),
        amount: 1500.0,
        requester: "john.doe@company.com".to_string(),
    };
    let request_json = serde_json::to_string(&request)?;

    let instance_id = "approval-instance-1";
    let client = DuroxideClient::new(store);
    client.start_orchestration(instance_id, "ApprovalWorkflow", request_json).await?;

    // Simulate an approval event after 2 seconds
    let rt_clone = rt.clone();
    let instance_id_clone = instance_id.to_string();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(2)).await;
        
        let approval = ApprovalResponse {
            request_id: "REQ-001".to_string(),
            approved: true,
            approver: "manager@company.com".to_string(),
            comments: "Approved for business expense".to_string(),
        };
        let approval_json = serde_json::to_string(&approval).unwrap();
        
        println!("🎯 Simulating approval event...");
        // Use client for control-plane in real apps; keep runtime for execution only.
        // For brevity we reuse runtime here to enqueue the event.
        rt_clone.raise_event(&instance_id_clone, "ApprovalEvent", approval_json).await;
    });

    match rt
        .wait_for_orchestration(instance_id, Duration::from_secs(10))
        .await
        .map_err(|e| format!("Wait error: {:?}", e))?
    {
        runtime::OrchestrationStatus::Completed { output } => {
            println!("✅ Approval workflow completed!");
            println!("Result: {}", output);
        }
        runtime::OrchestrationStatus::Failed { error } => {
            println!("❌ Workflow failed: {}", error);
        }
        _ => {
            println!("⏳ Workflow still running");
        }
    }

    rt.shutdown().await;
    Ok(())
}
