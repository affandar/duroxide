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

  // String IO
  pub fn schedule_activity(&self, name: impl Into<String>, input: impl Into<String>) -> DurableFuture
  pub fn schedule_timer(&self, delay_ms: u64) -> DurableFuture
  pub fn schedule_wait(&self, name: impl Into<String>) -> DurableFuture
  pub fn schedule_sub_orchestration(&self, name: impl Into<String>, input: impl Into<String>) -> DurableFuture
  pub fn schedule_orchestration(&self, name: impl Into<String>, instance: impl Into<String>, input: impl Into<String>)

  // Typed adapters
  pub fn schedule_activity_typed<In: Serialize, Out: DeserializeOwned>(&self, name: impl Into<String>, input: &In) -> DurableFuture
  pub fn schedule_wait_typed<T: DeserializeOwned>(&self, name: impl Into<String>) -> DurableFuture
  pub fn schedule_sub_orchestration_typed<In: Serialize, Out: DeserializeOwned>(&self, name: impl Into<String>, input: &In) -> DurableFuture

  // System helpers
  pub async fn system_now_ms(&self) -> u128
  pub async fn system_new_guid(&self) -> String

  // Control-flow
  pub fn continue_as_new(&self, input: impl Into<String>)
  pub fn continue_as_new_typed<In: Serialize>(&self, input: &In)

  // Deterministic aggregation
  pub fn select2(&self, a: DurableFuture, b: DurableFuture) -> SelectFuture
  pub fn select(&self, futures: Vec<DurableFuture>) -> SelectFuture
  pub fn join(&self, futures: Vec<DurableFuture>) -> JoinFuture

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
let child = ctx.schedule_sub_orchestration("Child", "x").into_sub_orchestration().await?;
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
  pub async fn start(activity_registry: Arc<ActivityRegistry>, orchestration_registry: OrchestrationRegistry) -> Arc<Self>
  pub async fn start_with_store(provider: Arc<dyn Provider>, activity_registry: Arc<ActivityRegistry>, orchestration_registry: OrchestrationRegistry) -> Arc<Self>
  pub async fn shutdown(&self)
}
```

### Providers

`Provider` abstracts storage and the multi-queue work API.
```rust
#[async_trait]
pub trait Provider {
  async fn read(&self, instance: &str) -> Vec<Event>;
  // multi-queue work API (peek-lock) and atomic orchestration commit live in the real trait in code
}
```

### Introspection

The runtime exposes a descriptor API to fetch orchestration metadata directly from history.

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrchestrationDescriptor {
  pub name: String,
  pub version: String,
  pub parent_instance: Option<String>,
  pub parent_id: Option<u64>,
}

impl Runtime {
  /// Returns the descriptor for an instance, or None if no history exists.
  pub async fn get_orchestration_descriptor(&self, instance: &str) -> Option<OrchestrationDescriptor>;
}
```

This reads the latest `OrchestrationStarted { name, version, input, parent_instance?, parent_id? }` from history and returns it.

### Example orchestrator

```rust
let orchestrator = |ctx: OrchestrationContext| async move {
  let a = ctx.schedule_activity("A", "1").into_activity().await?;
  let _ = ctx.schedule_timer(500).into_timer().await;
  let go = ctx.schedule_wait("Go").into_event().await;
  format!("a={a}, go={go}")
};
```


