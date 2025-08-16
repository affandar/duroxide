# Orchestration Registry and Runtime Resume

## Goal

Provide a first-class orchestration registry and enable the runtime to discover, load, and resume in-progress orchestration instances from the configured provider upon startup.

## Motivation

- Durable workflows often outlive a single process lifetime
- Restarting the runtime should not require re-submitting orchestration instances
- Align with Durable Task semantics for deterministic replay across restarts

## Requirements

- Orchestrator registration: map orchestration names to orchestrator functions
- Runtime startup resume:
  - Enumerate in-progress instances from the provider
  - For each instance, bind to the correct orchestrator implementation
  - Reconstruct in-memory state and continue deterministic replay
- Safety:
  - Avoid duplicate execution of the same instance
  - Idempotent operations across restarts
- Observability: log/tracing around discovery, resume, completion/failure

## Non-Goals (Initial)

- Multi-host sharding and distributed leases (may follow)
- Complex backpressure/flow-control policies beyond simple batching
- UI or visualization

## Terminology

- Orchestrator: async function taking `OrchestrationContext` and driving workflow
- Orchestration Registry: a mapping from orchestration name to orchestrator function
- Provider: persistence backend implementing `HistoryStore` for history and metadata

## High-Level Design

1. Orchestration Registry
   - Provide a registry builder to register orchestrator functions by name.
   - Validate uniqueness, expose lookup by orchestration name.

2. Provider Enumeration
   - Extend `HistoryStore` (or add an adapter) to list in-progress instances with metadata:
     - `instance_id`, `orchestrator_name`, `created_at`, `updated_at`, `status` (InProgress/Completed/Failed), `last_sequence`

3. Runtime Startup Resume
   - On `Runtime::start*`, if configured to resume, enumerate in-progress instances.
   - For each instance:
     - Lookup orchestrator by `orchestrator_name` in the registry.
     - Recover history, construct `OrchestrationContext`, and spawn to replay/continue.
   - Batch enumeration to avoid thundering herd; optional concurrency limit.

4. Safety & Concurrency
   - Single-run guarantee per instance via provider-level guard:
     - Option A: optimistic check on append with expected next index
     - Option B: advisory lock/lease with TTL (provider-specific)
   - Resume is idempotent: if already completed, runtime should detect and skip.

## Public API Sketch (Rust)

```rust
pub type OrchestratorFn = fn(OrchestrationContext) -> Pin<Box<dyn Future<Output = String> + Send>>;

#[derive(Default)]
pub struct OrchestrationRegistry {
    // name -> orchestrator
}

impl OrchestrationRegistry {
    pub fn builder() -> OrchestrationRegistryBuilder { /* ... */ }
    pub fn get(&self, name: &str) -> Option<OrchestratorFn> { /* ... */ }
}

pub struct OrchestrationRegistryBuilder { /* ... */ }
impl OrchestrationRegistryBuilder {
    pub fn register(mut self, name: &str, f: OrchestratorFn) -> Self { /* ... */ }
    pub fn build(self) -> OrchestrationRegistry { /* ... */ }
}

pub struct RuntimeOptions {
    pub resume_on_start: bool,
    pub max_concurrent_resumes: usize,
}

impl Runtime {
    pub async fn start_with_store_and_registries(
        store: Arc<dyn HistoryStore>,
        activities: Arc<ActivityRegistry>,
        orchestrators: Arc<OrchestrationRegistry>,
        options: RuntimeOptions,
    ) -> Arc<Runtime> { /* ... */ }
}
```

Notes:
- API names are illustrative; we can adapt to existing `Runtime::start_with_store` conventions or introduce a `RuntimeBuilder`.
- `OrchestratorFn` shape can leverage concrete function types or `Box<dyn Fn(...)>` depending on ergonomics.

## Provider Extensions

Minimal enumeration capability:

```rust
#[derive(Debug, Clone)]
pub struct InstanceMetadata {
    pub instance_id: String,
    pub orchestrator_name: String,
    pub status: InstanceStatus,
    pub created_at: u128,
    pub updated_at: u128,
}

#[async_trait]
pub trait HistoryStore {
    async fn list_in_progress(&self) -> anyhow::Result<Vec<InstanceMetadata>>;
    // existing APIs ...
}
```

FS provider can implement enumeration by scanning instance files and reading their terminal event.

## Resume Algorithm

- Enumerate `list_in_progress()` in batches
- For each metadata item:
  - Lookup orchestrator in registry; if missing, log error and skip
  - Load full history and reconstruct context
  - Spawn orchestration to completion, respecting deterministic replay
- Continue until backlog is drained

## Determinism & Correlation

- Align with the note in `src/lib.rs` regarding correlation IDs for races
- Buffer completions and correlate by stable IDs to avoid head-of-queue mismatches during replay

## Testing Strategy

- Unit tests: registry uniqueness, lookup, provider enumeration
- E2E tests:
  - Start instances, stop runtime mid-flight, restart and verify completion
  - Concurrency limits enforced during resume
  - Unknown orchestrator name handled gracefully

## Rollout

- Introduce behind a feature flag or opt-in `RuntimeOptions`
- Update samples to register orchestrators and demonstrate resume-on-start

## Open Questions

- Do we require provider leases or will optimistic append checks suffice for single-run?
- How to surface progress to operators (metrics/events)?
- Should registry support versioning and side-by-side deployments?

## Appendix

- Related Durable Task semantics notes
- Future work: distributed sharding, leases, UI