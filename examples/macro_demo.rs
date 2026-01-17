//! Macro Demo - Demonstrates the use of #[activity] and #[orchestration] macros
//!
//! Run with: `cargo run --example macro_demo`

use duroxide::providers::sqlite::SqliteProvider;
use duroxide::runtime::registry::{ActivityRegistry, OrchestrationRegistry};
use duroxide::runtime::{self};
use duroxide::{ActivityContext, Client, OrchestrationContext};
use duroxide::{activity, orchestration};
use std::sync::Arc;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
struct GreetInput {
    name: String,
}

// Activity defined with macro
#[activity]
async fn greet(ctx: ActivityContext, input: GreetInput) -> Result<String, String> {
    ctx.trace_info(format!("Greeting user: {}", input.name));
    Ok(format!("Hello, {}!", input.name))
}

// Orchestration defined with macro
#[orchestration]
async fn hello_world(ctx: OrchestrationContext, input: GreetInput) -> Result<String, String> {
    ctx.trace_info("Starting greeting orchestration");

    // Call the activity using the string name "greet"
    // In a future version, we could generate a client stub like `greet(ctx, input).await?`
    let greeting: String = ctx.schedule_activity_typed::<_, String>("greet", &input)
        .into_activity_typed()
        .await?;

    ctx.trace_info(format!("Greeting completed: {}", greeting));
    Ok(greeting)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing
    tracing_subscriber::fmt::init();

    // Setup DB
    let temp_dir = tempfile::tempdir()?;
    let db_path = temp_dir.path().join("macro_demo.db");
    std::fs::File::create(&db_path)?;
    let db_url = format!("sqlite:{}", db_path.to_str().unwrap());
    let store = Arc::new(SqliteProvider::new(&db_url, None).await?);

    // Auto-register activities and orchestrations using inventory
    let activities = ActivityRegistry::builder()
        .register_auto()
        .build();

    let orchestrations = OrchestrationRegistry::builder()
        .register_auto()
        .build();

    println!("Registered activities: {:?}", activities.list_names());
    println!("Registered orchestrations: {:?}", orchestrations.list_names());

    // Start runtime
    let rt = runtime::Runtime::start_with_store(store.clone(), Arc::new(activities), orchestrations).await;

    // Start orchestration
    let client = Client::new(store);
    let instance_id = "macro-instance-1";
    
    // We need to pass the input as a string that deserializes to GreetInput
    // Wait, Client::start_orchestration takes a string input.
    // The orchestration expects GreetInput deserialized from JSON.
    // So we should pass the JSON string.
    
    let input = GreetInput { name: "Macro User".to_string() };
    let input_json = serde_json::to_string(&input)?;
    
    client
        .start_orchestration(instance_id, "hello_world", &input_json)
        .await?;

    // Wait for completion
    match client
        .wait_for_orchestration(instance_id, std::time::Duration::from_secs(10))
        .await
        .map_err(|e| format!("Wait error: {e:?}"))?
    {
        duroxide::OrchestrationStatus::Completed { output } => {
            // output is JSON string "Hello, Macro User!"
            // Wait, result is Result<String, String>, serialized.
            // "Hello, Macro User!" serialized is "\"Hello, Macro User!\""
            // But the orchestration returns String.
            // Let's decode it.
            let result: String = serde_json::from_str(&output)?;
            println!("✅ Orchestration completed successfully!");
            println!("Result: {result}");
        }
        duroxide::OrchestrationStatus::Failed { details } => {
            println!("❌ Orchestration failed: {}", details.display_message());
        }
        _ => {
            println!("⏳ Orchestration still running or in unexpected state");
        }
    }

    rt.shutdown(None).await;
    Ok(())
}
