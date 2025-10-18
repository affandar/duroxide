//! Hello World with #[duroxide::main]
//!
//! Demonstrates the zero-ceremony #[duroxide::main] macro.
//!
//! Run with: `cargo run --example hello_world_duroxide_main`

use duroxide::prelude::*;

#[activity(typed)]
async fn greet(name: String) -> Result<String, String> {
    Ok(format!("Hello, {}!", name))
}

#[orchestration]
async fn hello_world(ctx: OrchestrationContext, name: String) -> Result<String, String> {
    durable_trace_info!("Greeting: {}", name);
    
    let greeting = durable!(greet(name)).await?;
    
    Ok(greeting)
}

// Phase 6: Zero-ceremony main! Everything auto-configured!
#[duroxide::main]
async fn main() {
    println!("🚀 Hello World with #[duroxide::main] (Phase 6)\n");
    
    // client is automatically available in scope!
    // Runtime is automatically started with auto-discovery!
    
    hello_world::start(&client, "hello-1", "Rust Developer".to_string())
        .await
        .unwrap();
    
    let result = hello_world::wait(&client, "hello-1", Duration::from_secs(10))
        .await
        .unwrap();
    
    match result {
        OrchestrationStatus::Completed { output } => {
            println!("✅ Result: {}", output);
        }
        OrchestrationStatus::Failed { error } => {
            println!("❌ Failed: {}", error);
        }
        _ => {
            println!("⏳ Still running...");
        }
    }
    
    println!("\n🎉 Zero ceremony - everything auto-configured!");
}

