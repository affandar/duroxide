//! Minimal memory baseline probes.
//!
//! Runs several increasingly-complex setups to isolate memory contributors.
//!
//! cargo run --example memory_baseline --features sqlite

#[cfg(not(feature = "sqlite"))]
fn main() {
    eprintln!("Requires --features sqlite");
}

#[cfg(feature = "sqlite")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::sync::Arc;
    use std::time::Duration;

    let mode = std::env::var("MODE").unwrap_or_else(|_| "help".into());

    match mode.as_str() {
        "bare" => {
            // Just tokio runtime + sleep — no duroxide at all
            eprintln!("[bare] tokio only");
            tokio::time::sleep(Duration::from_secs(999999)).await;
        }

        "tracing" => {
            // tokio + tracing subscriber
            eprintln!("[tracing] tokio + tracing_subscriber");
            let _ = tracing_subscriber::fmt().with_env_filter("warn").try_init();
            tokio::time::sleep(Duration::from_secs(999999)).await;
        }

        "sqlite_only" => {
            // tokio + tracing + SqliteProvider (no runtime)
            use duroxide::providers::sqlite::SqliteProvider;
            eprintln!("[sqlite_only] tokio + tracing + SqliteProvider::new_in_memory");
            let _ = tracing_subscriber::fmt().with_env_filter("warn").try_init();
            let _store = Arc::new(SqliteProvider::new_in_memory().await?);
            tokio::time::sleep(Duration::from_secs(999999)).await;
        }

        "runtime_no_sqlite" => {
            // Runtime::start() uses in-memory sqlite internally, but let's see the delta
            // Actually Runtime::start requires sqlite. Skip this variant.
            eprintln!("[runtime_no_sqlite] not applicable — Runtime requires a provider");
        }

        "full" => {
            // Full idle runtime (same as idle_runtime_footprint example)
            use duroxide::providers::sqlite::SqliteProvider;
            use duroxide::runtime::registry::ActivityRegistry;
            use duroxide::runtime::{self};
            use duroxide::OrchestrationRegistry;

            eprintln!("[full] full idle runtime");
            let _ = tracing_subscriber::fmt().with_env_filter("warn").try_init();

            let store = Arc::new(SqliteProvider::new_in_memory().await?);
            let activities = ActivityRegistry::builder().build();
            let orchestrations = OrchestrationRegistry::builder().build();
            let _rt = runtime::Runtime::start_with_store(store, Arc::new(activities), orchestrations).await;

            tokio::time::sleep(Duration::from_secs(999999)).await;
        }

        _ => {
            eprintln!("Usage: MODE=<bare|tracing|sqlite_only|full> cargo run --example memory_baseline --features sqlite");
            eprintln!("Then in another terminal: ps -p <pid> -o rss=");
        }
    }

    Ok(())
}
