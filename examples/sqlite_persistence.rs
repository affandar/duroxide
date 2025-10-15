/// Example demonstrating SQLite provider's automatic database file creation
/// and data persistence capabilities.
///
/// This example shows:
/// 1. Creating a database file automatically if it doesn't exist
/// 2. Preserving data when reopening an existing database
/// 3. Schema initialization is idempotent

use duroxide::providers::sqlite::SqliteProvider;
use duroxide::providers::{Provider, WorkItem};
use duroxide::Event;
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logging
    tracing_subscriber::fmt::init();

    // Use a database file in the current directory
    let db_path = PathBuf::from("./example_data.db");
    let db_url = format!("sqlite:{}", db_path.display());

    println!("=== SQLite Persistence Example ===\n");
    println!("Database path: {}", db_path.display());
    println!("Database exists before: {}\n", db_path.exists());

    // Phase 1: Create or open the database and add some data
    println!("Phase 1: Creating/opening database and adding data...");
    {
        // This will create the database file if it doesn't exist
        let provider = SqliteProvider::new(&db_url).await?;

        let instance_id = "example-instance-1";

        // Start an orchestration
        let start_work = WorkItem::StartOrchestration {
            instance: instance_id.to_string(),
            orchestration: "ExampleOrchestration".to_string(),
            version: Some("1.0.0".to_string()),
            input: r#"{"message": "Hello, SQLite!"}"#.to_string(),
            parent_instance: None,
            parent_id: None,
        };

        provider.enqueue_orchestrator_work(start_work, None).await?;

        // Fetch and process the work
        if let Some(item) = provider.fetch_orchestration_item().await {
            println!("  Fetched orchestration: {}", item.orchestration_name);

            // Add some history
            let history = vec![
                Event::OrchestrationStarted {
                    event_id: 1,
                    name: "ExampleOrchestration".to_string(),
                    version: "1.0.0".to_string(),
                    input: r#"{"message": "Hello, SQLite!"}"#.to_string(),
                    parent_instance: None,
                    parent_id: None,
                },
                Event::ActivityScheduled {
                    event_id: 2,
                    execution_id: 1,
                    name: "ExampleActivity".to_string(),
                    input: "activity-data".to_string(),
                },
            ];

            provider
                .ack_orchestration_item(
                    &item.lock_token,
                    1,
                    history,
                    vec![],
                    vec![],
                    vec![],
                    Default::default(),
                )
                .await?;

            println!("  Added orchestration history");
        }

        // Ensure data is written to disk
        provider.checkpoint().await?;
        println!("  Checkpointed database");
    } // Provider is dropped here

    println!("\nDatabase exists after Phase 1: {}", db_path.exists());

    // Phase 2: Re-open the database and verify data persists
    println!("\nPhase 2: Reopening database and verifying data...");
    {
        // Re-open the existing database
        let provider = SqliteProvider::new(&db_url).await?;

        // Read the history we added in Phase 1
        let history = provider.read("example-instance-1").await;

        println!("  Found {} history events:", history.len());
        for (i, event) in history.iter().enumerate() {
            match event {
                Event::OrchestrationStarted { name, input, .. } => {
                    println!("    {}. OrchestrationStarted: name={}, input={}", i + 1, name, input);
                }
                Event::ActivityScheduled { name, input, .. } => {
                    println!("    {}. ActivityScheduled: name={}, input={}", i + 1, name, input);
                }
                _ => {
                    println!("    {}. {:?}", i + 1, event);
                }
            }
        }
    }

    println!("\n=== Example completed successfully! ===");
    println!("The database file '{}' has been created and can be reused.", db_path.display());
    println!("Run this example again to see how it preserves the existing data.");

    Ok(())
}
