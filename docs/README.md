## Durable Task (Rust) – Documentation Home

This project is a minimal deterministic orchestration core inspired by Durable Task (DTF). It records an append-only history of `Event`s and replays the user’s orchestrator to make asynchronous workflows deterministic.

### High-level principles

- Determinism via replay: user logic (the orchestrator) is expressed as async code and is replayed against a stable history to make the same decisions every time.
- Append-only history: all progress is captured as `Event`s; hosts/providers only append, never mutate history.
- Single-poll-per-turn: the orchestrator is polled once per “turn,” producing `Action`s (schedule activity/timer/subscribe external) or finishing with an output.
- Correlation IDs: all schedule/subscribe operations have stable IDs; completions are matched by ID, not by the next event in the log.
- Host/Provider separation: the runtime and workers materialize `Action`s into `Event`s and persist them via a `HistoryStore` provider.

### What’s in here

- Architecture and execution model: see `architecture.md`
- API reference and usage: see `api.md`

### Quick start

- Define activities in a registry and start the in-process runtime.
- Implement an orchestrator as an async function using `OrchestrationContext` to schedule work.
- Drive a single instance to completion using the runtime (async) or the `Executor` (sync/test helper).


