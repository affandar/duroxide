## Hello World

This minimal example shows one activity and an orchestrator that calls it.

```rust
use std::sync::Arc;
use rust_dtf::{OrchestrationContext, OrchestrationRegistry};
use rust_dtf::runtime::{self, activity::ActivityRegistry};

#[tokio::main]
async fn main() {
    // Register a simple activity
    let activities = ActivityRegistry::builder()
        .register("Hello", |name: String| async move { Ok(format!("Hello, {name}!")) })
        .build();

    // Orchestrator: call Hello twice and return result using input
    let orchestration = |ctx: OrchestrationContext, input: String| async move {
        ctx.trace_info("hello_world started");
        let res = ctx.schedule_activity("Hello", "Rust").into_activity().await.unwrap();
        ctx.trace_info(format!("hello_world result={res}"));
        let res2 = ctx.schedule_activity("Hello", input).into_activity().await.unwrap();
        Ok(res2)
    };

    let registries = OrchestrationRegistry::builder()
        .register("HelloWorld", orchestration)
        .build();

    let rt = runtime::Runtime::start(Arc::new(activities), registries).await;
    let h = rt.clone().start_orchestration("inst-hello", "HelloWorld", "World").await.unwrap();
    let (_hist, out) = h.await.unwrap();
    println!("{}", out.unwrap()); // "Hello, World!"
    rt.shutdown().await;
}
```
