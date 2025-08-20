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
- External events semantics: see `external-events.md`
- ContinueAsNew semantics: see `continue-as-new.md`
- Active instances and gating: see `active-instances-and-gating.md`

### Proposals

- Typed API design: `proposals/typed-api-design.md`
- Typed futures (Unpin) adapters: `proposals/typed-futures-design.md`

### Quick start

- Define activities in a registry and start the in-process runtime.
- Implement an orchestrator as an async function using `OrchestrationContext` to schedule work.
- Drive a single instance to completion using the runtime (async) or the `Executor` (sync/test helper).


## Samples and Tests

Start with these end-to-end tests to learn the API and patterns by example:

- `tests/e2e_samples.rs` – documented “learning” samples
  - Hello world, control flow branching, loops and accumulation
  - Error handling and compensation
  - Parallel fan‑out/fan‑in (`futures::join`)
  - System activities (`system_now_ms`, `system_new_guid`)
  - Sub‑orchestrations: basic, fan‑out, and chained
  - Detached orchestration scheduling (fire‑and‑forget)
  - Mixed typed and string I/O samples, including `select!` over heterogeneous futures

- `tests/e2e_continue_as_new.rs` – ContinueAsNew scenarios
  - Multi‑execution rollover: orchestrator loops via `ctx.continue_as_new(new_input)`; provider stores all executions
  - External event routing to the latest execution
  - History assertions using execution‑aware provider APIs

You can run individual samples with:

```bash
cargo test --test e2e_samples -- --nocapture
cargo test --test e2e_continue_as_new -- --nocapture
```


## Maintaining docs

When changing behavior or adding new areas:
- Update affected docs and examples.
- Add new documents under `docs/` and link them here.


