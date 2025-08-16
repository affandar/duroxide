## Public API Overview

### Core types

- `Event`: append-only history entries; activity/timer/external/trace variants with correlation `id`.
- `Action`: decisions emitted from a turn; host/runtime materializes these into `Event`s.
- `OrchestrationContext`: user API surface during orchestration turns.
- `DurableFuture` and `DurableOutput`: unified future and its outputs for composition.
- `Executor`: synchronous helper to drive orchestrations turn-by-turn in tests.

### OrchestrationContext

```rust
pub struct OrchestrationContext { /* ... */ }
impl OrchestrationContext {
  pub fn now_ms(&self) -> u64
  pub fn new_guid(&self) -> String

  pub fn schedule_activity(&self, name: impl Into<String>, input: impl Into<String>) -> DurableFuture
  pub fn schedule_timer(&self, delay_ms: u64) -> DurableFuture
  pub fn schedule_wait(&self, name: impl Into<String>) -> DurableFuture

  pub fn trace_info(&self, msg: impl Into<String>)
  pub fn trace_warn(&self, msg: impl Into<String>)
  pub fn trace_error(&self, msg: impl Into<String>)
  pub fn trace_debug(&self, msg: impl Into<String>)
}
```

`schedule_*` functions return a `DurableFuture` which you can map into the concrete output:

```rust
let a = ctx.schedule_activity("Add", "1").into_activity().await?;
ctx.schedule_timer(100).into_timer().await;
let evt = ctx.schedule_wait("Go").into_event().await;
```

### Driving a turn

```rust
type TurnResult<O> = (Vec<Event>, Vec<Action>, Vec<(LogLevel, String)>, Option<O>);
fn run_turn<O, F>(history: Vec<Event>, orchestrator: impl Fn(OrchestrationContext) -> F) -> TurnResult<O>
where F: Future<Output = O>;
```

### Runtime

```rust
pub struct Runtime;
impl Runtime {
  pub async fn start(registry: Arc<ActivityRegistry>) -> Arc<Self>
  pub async fn start_with_store(history: Arc<dyn HistoryStore>, registry: Arc<ActivityRegistry>) -> Arc<Self>
  pub async fn run_instance_to_completion<O, F, OFut>(&self, instance: &str, orchestrator: F) -> (Vec<Event>, O)
  pub async fn spawn_instance_to_completion<O, F, OFut>(&self, instance: &str, orchestrator: F) -> JoinHandle<(Vec<Event>, O)>
  pub async fn raise_event(&self, instance: &str, name: impl Into<String>, data: impl Into<String>)
}
```

### Providers

`HistoryStore` abstracts storage; see the in-memory and filesystem-backed providers.

```rust
#[async_trait]
pub trait HistoryStore {
  async fn read(&self, instance: &str) -> Vec<Event>;
  async fn append(&self, instance: &str, new_events: Vec<Event>) -> Result<(), String>;
  async fn reset(&self);
  async fn list_instances(&self) -> Vec<String>;
  async fn dump_all_pretty(&self) -> String;
}
```

### Example orchestrator

```rust
let orchestrator = |ctx: OrchestrationContext| async move {
  let a = ctx.schedule_activity("A", "1").into_activity().await?;
  let _ = ctx.schedule_timer(500).into_timer().await;
  let go = ctx.schedule_wait("Go").into_event().await;
  format!("a={a}, go={go}")
};
```


