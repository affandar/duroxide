## Mixed String + Typed (Typed Orchestration)

Shows a typed orchestration that races a typed activity against a string activity using `select!`.

```rust
use std::sync::Arc;
use futures::{FutureExt, pin_mut, select};
use serde::{Serialize, Deserialize};
use rust_dtf::{OrchestrationContext, OrchestrationRegistry};
use rust_dtf::runtime::{self, activity::ActivityRegistry};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct AddReq { a: i32, b: i32 }
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct AddRes { sum: i32 }

#[tokio::main]
async fn main() {
    let activities = ActivityRegistry::builder()
        .register("Upper", |s: String| async move { Ok(s.to_uppercase()) })
        .register_typed::<AddReq, AddRes, _, _>("Add", |req| async move { Ok(AddRes { sum: req.a + req.b }) })
        .build();

    let orch = |ctx: OrchestrationContext, req: AddReq| async move {
        let f_typed = ctx
            .schedule_activity_typed::<AddReq, AddRes>("Add", &req)
            .into_activity_typed::<AddRes>()
            .map(|r| r.map(|v| format!("sum={}", v.sum)))
            .fuse();
        let f_str = ctx
            .schedule_activity("Upper", "hello")
            .into_activity()
            .map(|r| r.map(|v| format!("up={v}")))
            .fuse();
        pin_mut!(f_typed, f_str);
        let first = select! { a = f_typed => a, b = f_str => b };
        Ok::<_, String>(first.unwrap())
    };

    let regs = OrchestrationRegistry::builder()
        .register_typed::<AddReq, String, _, _>("MixedTypedOrch", orch)
        .build();

    let rt = runtime::Runtime::start(Arc::new(activities), regs).await;
    let h = rt.clone().start_orchestration_typed::<AddReq, String>("inst-mixed", "MixedTypedOrch", AddReq { a: 1, b: 2 }).await.unwrap();
    let (_hist, out) = h.await.unwrap();
    println!("{}", out.unwrap());
    rt.shutdown().await;
}
```
