## Architecture and Execution Model

Deterministic orchestration hinges on separating decision-making (user code) from side-effects (host/runtime), with all effects captured in an append-only history.

### Components

- Orchestrator (user code): async function polled once per turn. It reads history and requests new work via `Action`s.
- Runtime (in-process): executes activities and timers, routes external events, and appends resulting `Event`s. It runs three dispatchers that consume provider-backed queues (`WorkItem`) via peek-lock: OrchestrationDispatcher (Orchestrator queue), WorkDispatcher (Worker queue), and TimerDispatcher (Timer queue). Auto-resumes incomplete instances at startup.
- Provider: persistence boundary that stores history per instance (`Provider`).
- Workers/dispatchers: WorkDispatcher executes registered activities; TimerDispatcher manages timers (provider delayed-visibility or in-process fallback). No separate ActivityWorker component.

### High-level data flow

```mermaid
sequenceDiagram
    participant U as User Orchestrator
    participant R as Runtime
    participant OD as OrchestrationDispatcher
    participant WD as WorkDispatcher
    participant P as Provider (persistence + queues)

    U->>R: run_turn(history)
    R->>U: poll orchestrator once
    U-->>R: Decisions (ScheduleActivity/CreateTimer/Subscribe/Start...)
    R->>P: append Events (schedule/subscriptions)
    R->>WD: enqueue ActivityExecute (Worker queue)
    R->>P: enqueue TimerFired (Orchestrator queue with visible_at delay)
    WD->>P: enqueue ActivityCompleted/Failed (Orchestrator queue)
    OD->>R: deliver completions to orchestrator loop
    R->>P: append Events (completions)
    R->>U: next run_turn with updated history
```

### Event and Action model

```mermaid
classDiagram
    class Event {
      +ActivityScheduled(id, name, input)
      +ActivityCompleted(id, result)
      +ActivityFailed(id, error)
      +TimerCreated(id, fire_at_ms)
      +TimerFired(id, fire_at_ms)
      +ExternalSubscribed(id, name)
      +ExternalEvent(id, name, data)
      +OrchestrationStarted(name, input)
      +OrchestrationCompleted(output)
      +OrchestrationFailed(error)
      +OrchestrationChained(id, name, instance, input)
      +SubOrchestrationScheduled(id, name, instance, input)
      +SubOrchestrationCompleted(id, result)
      +SubOrchestrationFailed(id, error)
      +OrchestrationStarted(name, version, input, parent_instance?, parent_id?)
      +OrchestrationContinuedAsNew(input)
    }

    class Action {
      +CallActivity(id, name, input)
      +CreateTimer(id, delay_ms)
      +WaitExternal(id, name)
      +StartOrchestrationDetached(id, name, instance, input)
      +StartSubOrchestration(id, name, instance, input)
      +ContinueAsNew(input)
    }
```

### Turn execution

```mermaid
flowchart TD
    A[Start turn with history] --> B[Poll orchestrator once]
    B -->|Ready| C[Output captured]
    C --> H[Persist any new events + handle ContinueAsNew]
    H --> Z[Stop]
    B -->|Pending + Actions| D[Execute actions via runtime]
    D --> E[Workers complete]
    E --> F[Append Events]
    F --> G[Next turn]
```

### Races and correlation

- All schedule/subscribe ops allocate or adopt a correlation `id`.
- Completions are matched by `id` and buffered in history; composition via `select`/`join` is deterministic.
- We avoid relying on “next event in log” matching; multiple completions in one batch are safe.

### Multi-execution (ContinueAsNew)

- `ContinueAsNew` ends the current execution and starts a fresh one with new input.
- Providers persist all executions. Filesystem layout: `root/{instance}/{execution_id}.jsonl`.
- Runtime behavior:
  - Appends `OrchestrationContinuedAsNew` to the current execution.
  - Calls `create_new_execution` on the provider to create the next execution with `OrchestrationStarted { name, version, input, parent_* }`.
  - Enqueues a `StartOrchestration` work item and notifies waiters for the initial start with an empty success.
  - External events are routed to the latest execution.


