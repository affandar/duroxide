//! Fan-Out/Fan-In Pattern Example
//!
//! This example demonstrates parallel execution of multiple activities
//! and deterministic result aggregation - a common pattern for
//! processing multiple items concurrently.
//!
//! Now with macros for cleaner, type-safe code!
//!
//! Run with: `cargo run --example fan_out_fan_in`

use duroxide::prelude::*;
use duroxide::DurableOutput;
use serde::{Deserialize, Serialize};

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
#[activity(typed)]
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

#[activity(typed)]
async fn send_welcome_email(profile: UserProfile) -> Result<String, String> {
    // Simulate email sending
    tokio::time::sleep(Duration::from_millis(50)).await;
    Ok(format!("Welcome email sent to {}", profile.email))
}

// Fan-out/fan-in orchestration
#[orchestration]
async fn process_users(ctx: OrchestrationContext, users: Vec<User>) -> Result<String, String> {
    durable_trace_info!("Starting fan-out/fan-in for {} users", users.len());
    
    // Fan-out: Schedule all user profile fetches
    // Note: Must use ctx.join() for deterministic replay, not futures::try_join_all!
    let profile_futures: Vec<_> = users
        .into_iter()
        .map(|user| {
            // Serialize for schedule_activity (until typed client helpers support join)
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
    
    durable_trace_info!("✅ Fetched {} profiles", profiles.len());
    
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
    
    durable_trace_info!("✅ Sent {} emails", email_count);
    
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
    
    // Auto-discovery!
    let rt = Runtime::builder()
        .store(store.clone())
        .discover_activities()
        .discover_orchestrations()
        .start()
        .await;
    
    let client = Client::new(store);
    
    // Create test users
    let users = vec![
        User { id: 1, name: "Alice".to_string() },
        User { id: 2, name: "Bob".to_string() },
        User { id: 3, name: "Charlie".to_string() },
    ];
    
    println!("🚀 Processing {} users in parallel\n", users.len());
    
    // Use generated client helper
    process_users::start(&client, "fan-out-1", users).await?;
    
    // Wait with typed output
    match process_users::wait(&client, "fan-out-1", Duration::from_secs(10)).await? {
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
