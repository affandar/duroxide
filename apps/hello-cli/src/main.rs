use std::sync::Arc;
use rust_dtf::{OrchestrationContext, OrchestrationRegistry};
use rust_dtf::runtime::{Runtime, activity::ActivityRegistry};
use rust_dtf::providers::{HistoryStore};
use rust_dtf::providers::fs::FsHistoryStore;

async fn orchestrator(ctx: OrchestrationContext, _input: String) -> Result<String, String> {
    ctx.trace_info("hello-cli started1");
    let res = ctx.schedule_activity("Hello", "Rust").into_activity().await.unwrap();
    Ok(res)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Install a tracing subscriber to print logs; respects RUST_LOG if set
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .try_init();

    let activity_registry = ActivityRegistry::builder()
        .register("Hello", |name: String| async move { Ok(format!("Hello, {name}!")) })
        .build();

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("HelloOrchestration", orchestrator)
        .build();

    // Use filesystem-backed provider so history persists across runs
    let store = Arc::new(FsHistoryStore::new("./dtf-data", true)) as Arc<dyn HistoryStore>;
    let rt = Runtime::start_with_store(store, Arc::new(activity_registry), orchestration_registry).await;
    let handle = rt.clone().start_orchestration("inst-hello-cli-1", "HelloOrchestration", "").await.unwrap();
    let (_hist, output) = handle.await.unwrap();
    println!("{}", output.unwrap());
    rt.shutdown().await;
    Ok(())
}


