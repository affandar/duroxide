## Replay Engine Refactor: Session Summary and Next Steps

Date: 2025-09-15
Branch: `replay-engine-refactor`

### Purpose
Concise hand-off of the refactor to replace the legacy replay logic with the new replay engine, including current state, decisions, and the immediate next steps. This should be sufficient context to continue the work on another computer without revisiting prior chat history.

### Goals
- **Integrate the new replay engine** in `src/runtime/replay.rs` into mainline execution.
- **Provider-first migration**: make `ReplayHistoryEvent` the canonical history envelope at the provider boundary before changing the turn executor.
- **All tests pass** (unit + e2e) after integration.
- **Remove deprecated code** once validated.
- **No feature switches**: direct replacement, not a gradual rollout.
- **No test changes** unless strictly for debugging and only with explicit consultation.
- **Runtime-driven IDs**: runtime remains the single authority for `event_id` assignment and `scheduled_event_id` linkage.

### Key Concepts and Types
- **`ReplayHistoryEvent`**: `{ event_id: u64, scheduled_event_id: Option<u64>, event: Event }` — canonical history envelope.
- **`ReplayOrchestrationContext`** and **`ReplaySchedulingExt`**: wrapper/trait adding replay-aware scheduling methods.
- **`ReplayDurableFuture`**: replay-specific future tracking completion state.
- **`replay_orchestration`, `replay_turn`, `replay_core_with_future`**: new replay APIs; turn-based processing and per-event polling.
- **OpenFutures**: `Arc<Mutex<HashMap<u64, ReplayDurableFuture>>>` to track pending futures.

### Current Code State (high-level)
- `src/runtime/replay.rs` contains the new engine, heavily refactored:
  - Two-phase flow: baseline history then delta.
  - Polling moved inside per-event processing, with a loop to make progress until completion or stall.
  - Helpers: `set_future_ready`, `should_emit_decision`, `poll_and_collect`.
  - Test histories moved to JSON files under `src/runtime/test_data/` and loaded in tests.
- `src/runtime/event_ids.rs`:
  - `ReplayHistoryEvent` derives `Serialize`/`Deserialize`.
  - Removed unused `validate_wrapped_delta`.
- `src/lib.rs`:
  - `CtxInner::new` sets `next_correlation_id` to `1u64` to align with replayed histories.
- `src/runtime/replay_context_wrapper.rs`, `src/runtime/replay_methods.rs`:
  - Replay context wrapper and scheduling extension; debug prints removed.
- Provider (SQLite) currently stores `Vec<Event>` history and does not yet expose `ReplayHistoryEvent` nor first-class IDs in schema.

### Test Status
- Unit tests around replay largely green after fixes; histories now loaded from JSON test files.
- e2e: 4 failing in `tests/e2e_samples.rs` (to be revalidated post-provider migration):
  - `sample_hello_world_fs`
  - `sample_system_activities_fs`
  - `sample_versioning_sub_orchestration_explicit_vs_policy_fs`
  - `sample_nested_function_error_handling_fs`

### Notable Issues Found and Addressed
- **ID mismatch** between history and orchestration-generated IDs: fixed by starting `next_correlation_id` at 1.
- **Double polling** after completion causing `async fn resumed after completion`: guarded polling once complete.
- **Actions not collected** when polling too late: moved polling inside per-event loop.
- **Insufficient polling** for multi-step orchestrations: added inner progress loop in `poll_and_collect`.
- **`scheduled_event_id` vs variant `id`**: engine currently correlates via `Event` variant `id`; tests now populate `scheduled_event_id` for future use.
- **Test data readability**: moved to external JSON fixtures.
- **Legacy behavior parity**: preserved error for empty histories in `replay_orchestration`.

### Constraints and Migration Strategy
- Start by wiring `ReplayHistoryEvent` through the provider interface and storage (provider-first).
- Promote `event_id` and `scheduled_event_id` to first-class columns in the provider’s history table. No backward compatibility required in schema.
- Keep the runtime as the sole authority for `event_id` assignment and `scheduled_event_id` linkage.
- After provider migration, replace replay execution at `execute_orchestration()` level with the new engine.

### Provider Work (Phase 1–2)
- Trait changes in `src/providers/mod.rs`:
  - `OrchestrationItem.history: Vec<Event>` → `Vec<ReplayHistoryEvent>`.
  - Methods that consume/produce histories (`fetch_orchestration_item`, `ack_orchestration_item`, `read`, `read_with_execution`, `append_with_execution`) should use `ReplayHistoryEvent` for IO; conversion to `Event` happens at runtime boundaries where strictly necessary.
- SQLite schema changes in `src/providers/sqlite.rs` (and migrations):
  - History table to include:
    - `event_id INTEGER NOT NULL` (monotonic per execution)
    - `scheduled_event_id INTEGER NULL`
    - `event_type TEXT NOT NULL`
    - `event_data TEXT NOT NULL`
    - Suggested key: `(instance_id, execution_id, event_id)` as `PRIMARY KEY`.
  - All reads return rows as `ReplayHistoryEvent`.
  - All writes accept `ReplayHistoryEvent` produced by the runtime (runtime assigns `event_id`).

### Next Steps (Actionable)
1. Refactor provider trait to use `ReplayHistoryEvent` for history IO.
2. Update SQLite schema to add `event_id`, `scheduled_event_id` (make `(instance_id, execution_id, event_id)` the PK). Remove `sequence_num`.
3. Implement SQLite read/write to return/accept `Vec<ReplayHistoryEvent>` with the new columns.
4. Update runtime ingestion/emission to convert between `ReplayHistoryEvent` and `Event` only at the boundary as needed.
5. Add runtime ID allocator responsible for `event_id` assignment and `scheduled_event_id` linkage on completions.
6. Fix compile errors resulting from provider trait changes.
7. Re-run unit and e2e tests, investigate failures without modifying tests (unless temporarily for debugging).
8. Integrate new replay engine at `execute_orchestration()`; remove old/deprecated replay paths once green.
9. Cleanup: remove debug logging and unused constants; ensure consistent visibility and module boundaries.

### How to Continue (practical notes)
- Files to focus on:
  - `src/providers/mod.rs` (trait, `OrchestrationItem`)
  - `src/providers/sqlite.rs` (schema, read/write, ack path)
  - `src/runtime/event_ids.rs` (ID assignment helpers)
  - `src/runtime/replay.rs` (engine already implemented; integration target)
  - `src/runtime/orchestration_turn.rs` and `src/runtime/execution.rs` (integration points)
- Typical commands:
  - `cargo test -q`
  - `cargo test --test e2e_samples -q`
  - When using SQLite provider in tests, ensure in-memory mode or run migrations.

### Notes/Decisions
- No feature flags/switches for rollout.
- No permanent test edits without explicit discussion; logging is allowed for investigation.
- Provider schema is allowed to break back-compat as part of this refactor.

### Open TODOs (tracked in-session)
- Refactor provider trait to `ReplayHistoryEvent`.
- Add SQLite columns `event_id`, `scheduled_event_id` and rework PK.
- Implement SQLite read/write for `ReplayHistoryEvent`.
- Runtime conversion boundary and ID allocator.
- Compile fixes post-trait change.

If you need to pick up immediately, start with the provider trait refactor and schema update (Next Steps 1–3), then wire runtime boundary conversions (Step 4) and ID allocator (Step 5).
