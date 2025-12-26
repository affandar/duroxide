//! Idle runtime footprint probe.
//!
//! Starts a Duroxide runtime with an in-memory SQLite provider and no registered
//! orchestrations/activities, then stays alive for long enough to sample CPU/RSS.
//!
//! Run:
//!   cargo run --example idle_runtime_footprint --features sqlite

#[cfg(not(feature = "sqlite"))]
fn main() {
    eprintln!("This example requires the `sqlite` feature.\n\nRun: cargo run --example idle_runtime_footprint --features sqlite");
}

#[cfg(feature = "sqlite")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    use duroxide::runtime::registry::ActivityRegistry;
    use duroxide::runtime::{self};
    use duroxide::OrchestrationRegistry;
    use std::sync::Arc;
    use std::time::Duration;

    // Keep output minimal; sampling scripts generally redirect stdout/stderr anyway.
    let _ = tracing_subscriber::fmt().with_env_filter("warn").try_init();

    let activities = ActivityRegistry::builder().build();
    let orchestrations = OrchestrationRegistry::builder().build();

    let rt = runtime::Runtime::start(Arc::new(activities), orchestrations).await;

    // Run long enough for a warmup window + sampling window.
    // Sampling scripts can override this with DUROXIDE_IDLE_SECONDS.
    let seconds = std::env::var("DUROXIDE_IDLE_SECONDS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(75);

    tokio::time::sleep(Duration::from_secs(seconds)).await;

    rt.shutdown(None).await;
    Ok(())
}
