//! Fan-Out/Fan-In Pattern Example
//!
//! This example demonstrates parallel execution of multiple activities
//! and deterministic result aggregation - a common pattern for
//! processing multiple items concurrently.
//!
//! Now with macros for cleaner, type-safe code!
//!
//! Run with: `cargo run --example fan_out_fan_in --features macros`

use duroxide::providers::sqlite::SqliteProvider;
use duroxide::runtime::{self, Runtime, OrchestrationStatus};
use duroxide::{Client, OrchestrationContext, DurableOutput, OrchestrationRegistry};
use duroxide::runtime::registry::ActivityRegistry;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone, Serialize, Deserialize, Debug)]
struct User {
    id: u32,
    name: String,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
struct UserProfile {
    user_id: u32,
    email: String,
    preferences: Vec<String>,
}

// Activities with type safety
async fn fetch_user_profile(user: User) -> Result<UserProfile, String> {
    // Simulate API call delay
    tokio::time::sleep(Duration::from_millis(100)).await;
    
    let profile = UserProfile {
        user_id: user.id,
        email: format!("{}@example.com", user.name.to_lowercase()),
        preferences: vec!["notifications".to_string(), "dark_mode".to_string()],
    };
    
    Ok(profile)
}

async fn send_welcome_email(profile: UserProfile) -> Result<String, String> {
    // Simulate email sending
    tokio::time::sleep(Duration::from_millis(50)).await;
    Ok(format!("Welcome email sent to {}", profile.email))
}

// Fan-out/fan-in orchestration
async fn process_users(ctx: OrchestrationContext, users: Vec<User>) -> Result<String, String> {
    ctx.trace_info(format!("Starting fan-out/fan-in for {} users", users.len()));

    // Fan-out: Schedule all user profile fetches
    // Note: Must use ctx.join() for deterministic replay, not futures::try_join_all!
    let profile_futures: Vec<_> = users
        .into_iter()
        .map(|user| {
            let user_json = serde_json::to_string(&user).unwrap();
            ctx.schedule_activity("fetch_user_profile", user_json)
        })
        .collect();
    
    // Fan-in: Wait for all to complete deterministically
    let profile_results = ctx.join(profile_futures).await;
    
    // Extract results
    let mut profiles = Vec::new();
    for result in profile_results {
        match result {
            DurableOutput::Activity(Ok(profile_json)) => {
                let profile: UserProfile = serde_json::from_str(&profile_json).unwrap();
                profiles.push(profile);
            }
            DurableOutput::Activity(Err(e)) => return Err(e),
            _ => unreachable!(),
        }
    }
    
    ctx.trace_info(format!("✅ Fetched {} profiles", profiles.len()));
    
    // Send welcome emails
    let email_futures: Vec<_> = profiles
        .into_iter()
        .map(|profile| {
            let profile_json = serde_json::to_string(&profile).unwrap();
            ctx.schedule_activity("send_welcome_email", profile_json)
        })
        .collect();
    
    let email_results = ctx.join(email_futures).await;
    
    let mut email_count = 0;
    for result in email_results {
        match result {
            DurableOutput::Activity(Ok(_)) => email_count += 1,
            DurableOutput::Activity(Err(e)) => return Err(e),
            _ => unreachable!(),
        }
    }
    
    ctx.trace_info(format!("✅ Sent {} emails", email_count));
    
    Ok(format!("Processed {} users successfully", email_count))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();
    
    let temp_dir = tempfile::tempdir()?;
    let db_path = temp_dir.path().join("fan_out_fan_in.db");
    std::fs::File::create(&db_path)?;
    let db_url = format!("sqlite:{}", db_path.to_str().unwrap());
    let store = Arc::new(SqliteProvider::new(&db_url).await?);
    
    // Register activities
    let activities = ActivityRegistry::builder()
        .register("fetch_user_profile", |input: String| async move {
            let user: User = serde_json::from_str(&input).unwrap();
            let result = fetch_user_profile(user).await?;
            Ok(serde_json::to_string(&result).unwrap())
        })
        .register("send_welcome_email", |input: String| async move {
            let profile: UserProfile = serde_json::from_str(&input).unwrap();
            send_welcome_email(profile).await
        })
        .build();

    // Register orchestration
    let orchestrations = OrchestrationRegistry::builder()
        .register("process_users", |ctx: OrchestrationContext, input: String| async move {
            let users: Vec<User> = serde_json::from_str(&input).unwrap();
            process_users(ctx, users).await
        })
        .build();

    // Start runtime
    let rt = runtime::Runtime::start_with_store(store.clone(), Arc::new(activities), orchestrations).await;
    
    let client = Client::new(store);
    
    // Create test users
    let users = vec![
        User { id: 1, name: "Alice".to_string() },
        User { id: 2, name: "Bob".to_string() },
        User { id: 3, name: "Charlie".to_string() },
    ];
    
    println!("🚀 Processing {} users in parallel\n", users.len());
    
    // Start orchestration
    let users_json = serde_json::to_string(&users).unwrap();
    client.start_orchestration("fan-out-1", "process_users", users_json).await?;
    
    // Wait for completion
    match client.wait_for_orchestration("fan-out-1", Duration::from_secs(10)).await.unwrap() {
        OrchestrationStatus::Completed { output } => {
            println!("✅ {}", output);
        }
        OrchestrationStatus::Failed { error } => {
            println!("❌ Failed: {}", error);
        }
        _ => {
            println!("⏳ Still running...");
        }
    }
    
    rt.shutdown().await;
    
    println!("\n📝 This example uses macros for clean, type-safe code.");
    println!("For the low-level API, see: cargo run --example low_level_api");
    
    Ok(())
}
