## Provider Interface Cleanup Plan

### Phase 1 – Surface Rename Pass
- Rename `enqueue_orchestrator_work` → `enqueue_for_orchestrator`.
- Rename `enqueue_worker_work` → `enqueue_for_worker`.
- Rename `dequeue_worker_peek_lock` → `fetch_work_item`.
- Rename `ack_worker` → `ack_work_item`.
- Update all implementations, tests, helper methods, and docs to use the new names.
- Run `cargo check` to confirm no lingering references remain.

### Phase 2 – Management Layer Overhaul
- Rename the `ManagementCapability` trait to `ProviderManager`.
- Move `list_instances` and `list_executions` from `Provider` into `ProviderManager`.
- Update imports, module paths, docs, and call sites accordingly.
- Rename `read_execution` to `read_history_with_execution_id`.
- Add `read_history` that resolves the latest execution ID and delegates to `read_history_with_execution_id`.

### Phase 3 – Orchestration Fetch Semantics
- Change `Provider::fetch_orchestration_item` to return `Result<Option<OrchestrationItem>, ProviderError>`.
- Update runtime logic to propagate provider errors distinctly from “no work” responses.
- Adjust provider implementations (e.g., SQLite) and tests to use the new signature.

### Phase 4 – Execution ID Handling
- Remove `Provider::latest_execution_id`.
- Update runtime execution flows to rely on the execution ID passed into `run_single_execution_atomic`.
- Keep execution ID lookup only in `ProviderManager` for management scenarios.
- Re-run impacted tests, especially those touching management APIs.

### Phase 5 – Validation/Test Trait Split
- Introduce a public `ProviderValidator` trait exposing:
  - `read(&self, instance) -> Result<Vec<Event>, ProviderError>`
  - `read_with_execution(&self, instance, execution_id) -> Result<Vec<Event>, ProviderError>`
- Update validation harnesses to require `ProviderValidator` instead of the core `Provider` trait.
- Add a crate-private `ProviderTester` trait with `append_with_execution` for internal tests.
- Gate `ProviderTester` usage to Duroxide’s own test modules.

### Phase 6 – Implementation Updates
- Implement `Provider`, `ProviderManager`, `ProviderValidator`, and `ProviderTester` for `SqliteProvider`.
- Ensure async trait bounds are satisfied (`async_trait` or manual futures as needed).
- Update all tests, helpers, and docs to use the new trait names and method signatures.
- Document the recommended trait adoption order (core → manager → validator → tester).

### Phase 7 – Validation & Cleanup
- Run `cargo fmt`, `cargo clippy`, and `cargo test --all` to ensure the codebase is clean.
- Verify public re-exports expose `Provider`, `ProviderManager`, and `ProviderValidator` appropriately.
- Prepare migration notes summarizing the breaking changes for downstream providers.

