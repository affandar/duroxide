use std::sync::Arc;
use rust_dtf::OrchestrationContext;
use rust_dtf::runtime::{Runtime, activity::ActivityRegistry};
use rust_dtf::providers::{HistoryStore};
use rust_dtf::providers::fs::FsHistoryStore;

async fn orchestrator(ctx: OrchestrationContext) -> String {
    ctx.trace_info("hello-cli started1");
    let res = ctx.schedule_activity("Hello", "Rust").into_activity().await.unwrap();
    res
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Install a tracing subscriber to print logs; respects RUST_LOG if set
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .try_init();

    let registry = ActivityRegistry::builder()
        .register_result("Hello", |name: String| async move { Ok(format!("Hello, {name}!")) })
        .build();
    // Use filesystem-backed provider so history persists across runs
    let store = Arc::new(FsHistoryStore::new("./dtf-data", true)) as Arc<dyn HistoryStore>;
    let rt = Runtime::start_with_store(store, Arc::new(registry)).await;
    let handle = rt.clone().spawn_instance_to_completion("inst-hello-cli-1", orchestrator).await;
    let (_hist, output) = handle.await.unwrap();
    println!("{output}");
    rt.shutdown().await;
    Ok(())
}


