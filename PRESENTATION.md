

# Duroxide: Deterministic Task Orchestration in Rust

---

## Agenda

- Basic workflow concepts for code-based workflow engines
- Mechanics of replay
- How replay is implemented in Duroxide
- Sample orchestrations

---

## Basic Concepts: Code-Based Workflow Engines

- **Orchestrations**: async functions defining workflow logic; must be deterministic
- **Activities**: stateless side-effecting work (I/O, APIs, DB); retried by the runtime
- **Timers**: durable delays/timeouts; never block threads
- **External events**: signals from the outside world matched by name
- **History**: append-only log of decisions and outcomes enabling replay
- **Correlation IDs**: bind an await to its scheduled work and completion

---

## Determinism Rules (Do & Avoid)

- **Do**: pure control flow in orchestrations, schedule work via context methods
- **Do**: put all side effects in activities
- **Do**: use timers for all delays and deadlines
- **Avoid**: non-deterministic APIs in orchestration code (random, time, I/O)
- **Avoid**: sleeping/waiting in activities to simulate delays (use timers in orchestrations)

---

## Mechanics of Replay (General)

- **Idea**: Orchestrations rebuild state by re-executing code against prior history
- **Scheduling vs completion**:
  - First execution: schedule work and record events
  - Replays: do not reschedule; consume existing completions from history
- **Await mapping**: each await correlates to a prior completion by ID (or name for external events)
- **Outcome**: Same inputs + same history => same decisions every time

---

## Replay: Failure and Recovery

- **Crash during execution**: runtime restarts, replays history, resumes at next await
- **Idempotence**: activity side effects occur once; duplicative scheduling is avoided by replay
- **Races**: select/join must resolve deterministically from persisted completion order

---

## Duroxide: Data Model

- **Events** (append-only history):
  - `OrchestrationStarted`, `OrchestrationCompleted`/`Failed`
  - `ActivityScheduled` → `ActivityCompleted`/`Failed`
  - `TimerCreated` → `TimerFired`
  - `ExternalSubscribed` ↔ `ExternalEvent`
  - `SubOrchestrationScheduled` → `SubOrchestrationCompleted`/`Failed`
  - `SystemCall` (e.g., time, guid, trace)
- **Actions** (host decisions):
  - `CallActivity`, `CreateTimer`, `WaitExternal`
  - `StartSubOrchestration`, `StartOrchestrationDetached`
  - `ContinueAsNew`, `SystemCall`

---

## Duroxide: OrchestrationContext API

- Schedule work:
  - `schedule_activity`, `schedule_timer`, `schedule_wait`
  - `schedule_sub_orchestration`, `schedule_orchestration`
  - `continue_as_new`, `trace_*`
- Futures pattern:
  - All schedule calls return `DurableFuture`
  - Convert before awaiting: `.into_activity()`, `.into_timer()`, `.into_event()`, `.into_sub_orchestration()`
  - Compose with `select2`/`select` and `join`

---

## Duroxide: Replay Implementation (Core Loop)

- **Single-turn executor**:
  - `run_turn_with(history, turn_index, execution_id, orchestrator)` polls the orchestration once
  - Produces updated `history`, `actions`, and optional `output`
- **DurableFuture internals**:
  - On first poll, claim or create the matching scheduling event (`claimed_scheduling_events`)
  - Search history for the correlated completion; enforce FIFO consumption (`consumed_completions`)
  - Set nondeterminism hints if schedule order mismatches are detected
- **System calls**:
  - `utcnow_ms`, `guid`, `trace_*` use `SystemCall` events to persist computed values and replay safely

---

## Duroxide: Runtime and Queues

- **Dispatchers**:
  - Orchestrator dispatcher: pulls messages, runs atomic turns, appends history
  - Work dispatcher: executes activities and enqueues `ActivityCompleted`/`Failed`
  - Timer dispatcher: schedules `TimerFired` with delayed visibility (provider-backed)
- **Provider (SQLite)**:
  - Persists history and queues atomically with peek-lock semantics
  - Supports delayed visibility for timers for accurate wakeups

---

## Sample: Hello World

```rust
use duroxide::{OrchestrationContext, OrchestrationRegistry};
use duroxide::runtime::{self};
use duroxide::runtime::registry::ActivityRegistry;
use duroxide::providers::sqlite::SqliteProvider;
use std::sync::Arc;

# #[tokio::main]
# async fn main() {
let store = Arc::new(SqliteProvider::new("sqlite:./data.db").await.unwrap());
let activities = ActivityRegistry::builder()
    .register("Hello", |name: String| async move { Ok(format!("Hello, {name}!")) })
    .build();

let orch = |ctx: OrchestrationContext, name: String| async move {
    let res = ctx.schedule_activity("Hello", name).into_activity().await?;
    Ok::<_, String>(res)
};

let orchestrations = OrchestrationRegistry::builder().register("HelloWorld", orch).build();
let rt = runtime::Runtime::start_with_store(store.clone(), Arc::new(activities), orchestrations).await;
rt.clone().start_orchestration("inst-hello-1", "HelloWorld", "Rust").await.unwrap();
# rt.shutdown().await;
# }
```

---

## Sample: Fan-Out/Fan-In

```rust
use duroxide::{OrchestrationContext, DurableOutput};

async fn fanout(ctx: OrchestrationContext, items: Vec<String>) -> Result<String, String> {
    let futures = items.into_iter()
        .map(|it| ctx.schedule_activity("ProcessItem", it))
        .collect::<Vec<_>>();

    let results = ctx.join(futures).await; // deterministic history order
    let successes = results.into_iter().filter(|o| matches!(o, DurableOutput::Activity(Ok(_)))).count();
    Ok(format!("Processed {successes} items"))
}
```

---

## Sample: Human-in-the-Loop with Timeout

```rust
use duroxide::{OrchestrationContext, DurableOutput};

async fn approval(ctx: OrchestrationContext) -> String {
    let timeout = ctx.schedule_timer(30_000);
    let approve = ctx.schedule_wait("ApprovalEvent");
    let (_idx, out) = ctx.select2(timeout, approve).await;

    match out {
        DurableOutput::External(data) => data,     // approved by human
        DurableOutput::Timer => "timeout".into(),  // fallback path
        _ => "unexpected".into(),
    }
}
```

---

## Sample: Error Handling and Compensation

```rust
use duroxide::OrchestrationContext;

async fn compensating(ctx: OrchestrationContext) -> String {
    match ctx.schedule_activity("Fragile", "bad").into_activity().await {
        Ok(v) => v,
        Err(_e) => ctx.schedule_activity("Recover", "").into_activity().await.unwrap(),
    }
}
```

---

## Sample: ContinueAsNew (Periodic Processing)

```rust
use duroxide::OrchestrationContext;

async fn periodic(ctx: OrchestrationContext, next_cursor: String) -> Result<(), String> {
    let processed = ctx.schedule_activity("ProcessBatch", next_cursor).into_activity().await?;
    // Decide whether to continue with the next page/cursor
    ctx.continue_as_new(processed);
    Ok(())
}
```

---

## Good Practices

- **Keep orchestrations deterministic**: only schedule and coordinate
- **Keep activities idempotent**: tolerate retries
- **Use timers** for delays and backoffs
- **Use external events** for human/workflow callbacks
- **Leverage traces** via `ctx.trace_*` for replay-safe logging

---

## Q&A

- Explore `src/lib.rs`, `src/futures.rs`, and `src/runtime/` for deeper details
- See `examples/` and `tests/e2e_samples.rs` for runnable patterns

---

## Visual: Core components and message flow

```mermaid
graph TB
    subgraph Orchestration["Orchestration (deterministic)"]
        UC[User Code<br/>async fn] --> DF[DurableFuture]
        DF --> Actions[Actions<br/>CallActivity<br/>CreateTimer<br/>WaitExternal]
    end
    
    Actions --> WQ[Worker Queue]
    Actions --> TQ[Timer Queue]
    Actions --> OQ[Orchestrator Queue]
    
    subgraph Runtime["Runtime (background workers)"]
        WD[Work Dispatcher]
        TD[Timer Dispatcher]
        OD[Orchestrator Dispatcher]
    end
    
    WQ --> WD
    TQ --> TD
    OQ --> OD
    
    WD --> OQ
    TD --> OQ
    
    subgraph Provider["Provider (SQLite)"]
        H[(History<br/>append-only)]
        AC[Atomic Commits<br/>txn guarantees]
    end
    
    OD --> H
    H --> AC
    AC --> Orchestration
```

---

## Visual: Replay mechanism (first run vs replay)

```mermaid
sequenceDiagram
    autonumber
    participant O as Orchestration
    participant H as History Store
    participant W as Worker
    
    Note over O,W: FIRST EXECUTION
    O->>H: Schedule ActivityScheduled(id=42)
    O->>W: Enqueue Execute(id=42)
    W->>W: Execute activity
    W->>H: Append ActivityCompleted(source=42)
    Note over O: Poll::Pending until completion
    
    Note over O,W: REPLAY (after crash/restart)
    O->>H: Read history
    H-->>O: Returns existing:<br/>[ActivityScheduled id=42]<br/>[ActivityCompleted source=42]
    O->>O: Claim id=42 from history<br/>(no new event)
    O->>O: Await matches completion<br/>(no worker execution)
    Note over O: Poll::Ready("ok")
```

---

## Visual: Correlation and FIFO consumption

```mermaid
graph TD
    subgraph History["History (append-only, ordered by event_id)"]
        S1["event 10: ActivityScheduled<br/>(name='Task1')"]
        S2["event 11: TimerCreated<br/>(delay=5000ms)"]
        C1["event 101: ActivityCompleted<br/>(source=10, result='ok')"]
        C2["event 102: TimerFired<br/>(source=11)"]
    end
    
    S1 -.correlates.-> C1
    S2 -.correlates.-> C2
    
    subgraph Rules["FIFO Consumption Rules"]
        R["Must consume event 101 BEFORE 102<br/>(even if timer polls first,<br/>it waits for earlier completions)"]
    end
    
    C1 --> R
    C2 --> R
```

---

## Visual: select/join determinism

```mermaid
sequenceDiagram
    participant Code as Orchestration Code
    participant H as History
    
    Note over Code: let fut_a = schedule_activity("A")<br/>let fut_b = schedule_activity("B")<br/>let (winner, output) = select2(fut_a, fut_b)
    
    Code->>H: Schedule A (id=20)
    Code->>H: Schedule B (id=21)
    
    Note over H: Activities execute<br/>B completes first!
    H->>H: event 299: ActivityCompleted(source=21, "B done")
    H->>H: event 300: ActivityCompleted(source=20, "A done")
    
    Note over Code,H: select2 resolves
    H-->>Code: winner=1 (fut_b)<br/>eid 299 < 300<br/>output="B done"
    
    Note over Code: join would return:<br/>["B done", "A done"]<br/>(sorted by completion eid)
```

---

## Visual: Human-in-the-loop (external events)

```mermaid
sequenceDiagram
    autonumber
    participant O as Orchestration
    participant H as History
    participant E as External System/Human
    
    O->>H: schedule_wait("ApprovalEvent")
    H->>H: Append ExternalSubscribed
    Note over O: Poll::Pending<br/>Waiting for signal...
    
    Note over E: Human clicks "Approve"
    E->>H: Runtime.raise_event()<br/>("ApprovalEvent", data)
    H->>H: Append ExternalEvent<br/>("ApprovalEvent", data)
    
    Note over O,H: Next turn/replay
    O->>H: Read history
    H-->>O: Match by NAME<br/>(not source_id!)
    O->>O: Poll::Ready(data)
    
    Note over O,H: External events match by NAME<br/>ExternalSubscribed can match<br/>multiple ExternalEvents
```

---

## Visual: Timers and delayed visibility

```mermaid
sequenceDiagram
    autonumber
    participant O as Orchestration
    participant H as History
    participant R as Runtime
    participant TD as Timer Dispatcher
    
    O->>H: schedule_timer(5000ms)
    H->>H: Append TimerCreated<br/>(id=55, fireAt=now+5000)
    R->>TD: Enqueue to Timer Queue<br/>(visibility_delay=5000ms)
    
    Note over TD: Item not visible yet...<br/>⏱️ 5 seconds pass
    
    TD->>TD: Timer becomes visible
    TD->>H: Append TimerFired(source=55)
    TD->>R: Enqueue to Orchestrator Queue
    
    Note over O,R: Next turn
    O->>H: Read history
    H-->>O: TimerFired(source=55)
    O->>O: Poll::Ready(())
```

---

## Visual: ContinueAsNew lifecycle

```mermaid
graph TB
    subgraph Exec1["Execution 1 (execution_id=1)"]
        E1S[OrchestrationStarted<br/>input='page1']
        E1A[ActivityScheduled → Completed]
        E1C[OrchestrationContinuedAsNew<br/>input='page2'<br/>TERMINAL]
        E1S --> E1A --> E1C
    end
    
    E1C -.Runtime creates new execution.-> E2S
    
    subgraph Exec2["Execution 2 (execution_id=2)<br/>Same instance='order-123'"]
        E2S[OrchestrationStarted<br/>input='page2'<br/>FRESH START]
        E2A[ActivityScheduled → Completed]
        E2C[OrchestrationCompleted<br/>output='done']
        E2S --> E2A --> E2C
    end
    
    Note["Use case: periodic tasks,<br/>pagination, eternal workflows"]
```

---

## Visual: Fan-out/Fan-in pattern

```mermaid
sequenceDiagram
    participant O as Orchestration
    participant H as History
    participant W as Workers (parallel)
    
    Note over O: Turn 1: Fan-out
    O->>H: Schedule Activity("Process", "item1") → id=10
    O->>H: Schedule Activity("Process", "item2") → id=11
    O->>H: Schedule Activity("Process", "item3") → id=12
    O->>O: ctx.join([10,11,12])<br/>Poll::Pending
    
    Note over W: Workers execute in parallel
    W->>H: ActivityCompleted(source=11, "B") eid=101
    W->>H: ActivityCompleted(source=10, "A") eid=102
    W->>H: ActivityCompleted(source=12, "C") eid=103
    
    Note over O: Turn 2: Fan-in
    O->>H: Read history
    H-->>O: join() resolves<br/>["B", "A", "C"]<br/>(ordered by completion eid)
    O->>O: Aggregate results<br/>and continue
```



