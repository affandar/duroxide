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
  pub async fn start_with_store(history: Arc<dyn HistoryStore>, activity_registry: Arc<ActivityRegistry>, orchestration_registry: OrchestrationRegistry) -> Arc<Self>
  pub async fn start_orchestration(&self: Arc<Self>, instance: &str, orchestration_name: &str, input: impl Into<String>) -> Result<JoinHandle<(Vec<Event>, Result<String, String>)>, String>
  pub async fn start_orchestration_typed<In: Serialize, Out: DeserializeOwned>(&self: Arc<Self>, instance: &str, orchestration_name: &str, input: In) -> Result<JoinHandle<(Vec<Event>, Result<Out, String>)>, String>
  pub async fn get_orchestration_status(&self, instance: &str) -> OrchestrationStatus
  pub async fn get_orchestration_status_latest(&self, instance: &str) -> OrchestrationStatus
  pub async fn get_orchestration_status_with_execution(&self, instance: &str, execution_id: u64) -> OrchestrationStatus
  pub async fn list_executions(&self, instance: &str) -> Vec<u64>
  pub async fn get_execution_history(&self, instance: &str, execution_id: u64) -> Vec<Event>
  pub async fn raise_event(&self, instance: &str, name: impl Into<String>, data: impl Into<String>)
}
```

### Providers

`HistoryStore` abstracts storage and the multi-queue work API.
```rust
#[async_trait]
pub trait HistoryStore {
  async fn read(&self, instance: &str) -> Vec<Event>;
  async fn append(&self, instance: &str, new_events: Vec<Event>) -> Result<(), String>;
  async fn reset(&self);
  async fn list_instances(&self) -> Vec<String>;
  async fn dump_all_pretty(&self) -> String;
  // instance lifecycle
  async fn create_instance(&self, instance: &str) -> Result<(), String>;
  async fn remove_instance(&self, instance: &str) -> Result<(), String>;
  // multi-queue work API (peek-lock)
  async fn enqueue_work(&self, kind: QueueKind, item: WorkItem) -> Result<(), String>;
  async fn dequeue_peek_lock(&self, kind: QueueKind) -> Option<(WorkItem, String)>;
  async fn ack(&self, kind: QueueKind, token: &str) -> Result<(), String>;
  async fn abandon(&self, kind: QueueKind, token: &str) -> Result<(), String>;
  // multi-execution (ContinueAsNew)
  async fn latest_execution_id(&self, instance: &str) -> Option<u64>;
  async fn list_executions(&self, instance: &str) -> Vec<u64>;
  async fn read_with_execution(&self, instance: &str, execution_id: u64) -> Vec<Event>;
  async fn append_with_execution(&self, instance: &str, execution_id: u64, new_events: Vec<Event>) -> Result<(), String>;
  async fn reset_for_continue_as_new(&self, instance: &str, orchestration: &str, version: &str, input: &str, parent_instance: Option<&str>, parent_id: Option<u64>) -> Result<u64, String>;
}

pub enum QueueKind { Orchestrator, Worker, Timer }
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


