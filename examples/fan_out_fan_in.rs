//! Fan-Out/Fan-In Pattern Example
//!
//! This example demonstrates parallel execution of multiple activities
//! and deterministic result aggregation - a common pattern for
//! processing multiple items concurrently.
//!
//! Run with: `cargo run --example fan_out_fan_in`

use duroxide::providers::sqlite::SqliteHistoryStore;
use duroxide::runtime::registry::ActivityRegistry;
use duroxide::runtime::{self};
use duroxide::{OrchestrationContext, OrchestrationRegistry, DurableOutput, DuroxideClient};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Serialize, Deserialize, Debug)]
struct User {
    id: u32,
    name: String,
}

#[derive(Serialize, Deserialize, Debug)]
struct UserProfile {
    user_id: u32,
    email: String,
    preferences: Vec<String>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let temp_dir = tempfile::tempdir()?;
    let db_path = temp_dir.path().join("fan_out_fan_in.db");
    std::fs::File::create(&db_path)?;
    let db_url = format!("sqlite:{}", db_path.to_str().unwrap());
    let store = Arc::new(SqliteHistoryStore::new(&db_url).await?);

    // Register activities for user processing
    let activities = ActivityRegistry::builder()
        .register("FetchUserProfile", |user_json: String| async move {
            let user: User = serde_json::from_str(&user_json)
                .map_err(|e| format!("JSON parse error: {}", e))?;
            // Simulate API call delay
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            
            let profile = UserProfile {
                user_id: user.id,
                email: format!("{}@example.com", user.name.to_lowercase()),
                preferences: vec!["notifications".to_string(), "dark_mode".to_string()],
            };
            Ok(serde_json::to_string(&profile)
                .map_err(|e| format!("JSON serialize error: {}", e))?)
        })
        .register("SendWelcomeEmail", |profile_json: String| async move {
            let profile: UserProfile = serde_json::from_str(&profile_json)
                .map_err(|e| format!("JSON parse error: {}", e))?;
            // Simulate email sending
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            Ok(format!("Welcome email sent to {}", profile.email))
        })
        .build();

    // Fan-out/fan-in orchestration
    let orchestration = |ctx: OrchestrationContext, users_json: String| async move {
        ctx.trace_info("Starting fan-out/fan-in orchestration");
        
        // Parse input users
        let users: Vec<User> = serde_json::from_str(&users_json)
            .map_err(|e| format!("JSON parse error: {}", e))?;
        ctx.trace_info(format!("Processing {} users in parallel", users.len()));

        // Fan-out: Schedule all user profile fetches in parallel
        let profile_futures: Vec<_> = users
            .into_iter()
            .map(|user| {
                let user_json = serde_json::to_string(&user).unwrap();
                ctx.schedule_activity("FetchUserProfile", user_json)
            })
            .collect();

        // Fan-in: Wait for all profiles to complete (deterministic order)
        let profile_results = ctx.join(profile_futures).await;
        
        // Process results and send welcome emails
        let mut email_results = Vec::new();
        for result in profile_results {
            match result {
                DurableOutput::Activity(Ok(profile_json)) => {
                    let profile: UserProfile = serde_json::from_str(&profile_json)
                        .map_err(|e| format!("JSON parse error: {}", e))?;
                    ctx.trace_info(format!("Fetched profile for user {}", profile.user_id));
                    
                    // Schedule welcome email
                    let email_future = ctx.schedule_activity("SendWelcomeEmail", profile_json);
                    email_results.push(email_future);
                }
                DurableOutput::Activity(Err(e)) => {
                    ctx.trace_error(format!("Failed to fetch user profile: {}", e));
                    return Err(format!("Profile fetch failed: {}", e));
                }
                _ => return Err("Unexpected result type".to_string()),
            }
        }

        // Wait for all emails to be sent
        let email_completions = ctx.join(email_results).await;
        let mut success_count = 0;
        
        for result in email_completions {
            match result {
                DurableOutput::Activity(Ok(message)) => {
                    ctx.trace_info(message);
                    success_count += 1;
                }
                DurableOutput::Activity(Err(e)) => {
                    ctx.trace_error(format!("Email failed: {}", e));
                }
                _ => {}
            }
        }

        Ok(format!("Successfully processed {} users", success_count))
    };

    let orchestrations = OrchestrationRegistry::builder()
        .register("FanOutFanIn", orchestration)
        .build();

    let rt = runtime::DuroxideRuntime::start_with_store(
        store.clone(),
        Arc::new(activities),
        orchestrations,
    ).await;

    // Test data: multiple users to process
    let users = vec![
        User { id: 1, name: "Alice".to_string() },
        User { id: 2, name: "Bob".to_string() },
        User { id: 3, name: "Charlie".to_string() },
    ];
    let users_json = serde_json::to_string(&users)?;

    let instance_id = "fan-out-instance-1";
    let client = DuroxideClient::new(store);
    client.start_orchestration(instance_id, "FanOutFanIn", users_json).await?;

    match client
        .wait_for_orchestration(instance_id, std::time::Duration::from_secs(10))
        .await
        .map_err(|e| format!("Wait error: {:?}", e))?
    {
        duroxide::OrchestrationStatus::Completed { output } => {
            println!("✅ Fan-out/fan-in orchestration completed!");
            println!("Result: {}", output);
        }
        duroxide::OrchestrationStatus::Failed { error } => {
            println!("❌ Orchestration failed: {}", error);
        }
        _ => {
            println!("⏳ Orchestration still running");
        }
    }

    rt.shutdown().await;
    Ok(())
}
