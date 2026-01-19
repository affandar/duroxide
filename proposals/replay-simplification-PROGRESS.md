# Replay Simplification – Rolling Progress

**Spec:** [proposals/replay-simplification.md](replay-simplification.md)  
**Checkpoint commit:** `555c5751653ac770a94d3962f743b7a2d7f6ad58` (revert here if approach fails)

---

## Current State (2026-01-19 - Update 5)

### What's Working

**Phase 1 harness** - Test-only simplified replay engine validating commands-vs-history model:
- 8 unit tests covering: schedule mismatch, completion validation, kind-checking, external event ordering, terminal history handling
- Location: `src/runtime/replay_engine_simplified.rs`
- Run: `cargo test replay_engine_simplified --lib`

**Modal switch infrastructure** - Added to production code:
- `ReplayMode` enum (`Legacy` / `Simplified`) in `CtxInner`
- State fields: `simplified_next_token`, `simplified_emitted`, `simplified_results`, `simplified_bindings`
- Helper methods: `emit_action()`, `drain_emitted_actions()`, `bind_token()`, `deliver_result()`, `get_result()`
- `simplified_token` field added to `DurableFuture`

**Schedule-time action emission**:
- `schedule_activity()`, `schedule_timer()`, `schedule_wait()`, `schedule_sub_orchestration()`, `schedule_system_call()` emit actions at schedule time in simplified mode
- `poll()` only checks results map in simplified mode

**execute_orchestration_simplified()** - Full event-processing loop:
- `use_simplified_replay` option in `RuntimeOptions` (default: false)
- Creates context in `ReplayMode::Simplified`
- Processes history events in order
- Matches emitted actions against schedule events
- Binds tokens to schedule_ids
- Delivers completions to results map
- Uses `prep_completions()` for both modes (unified completion handling)
- Re-polls on progress
- Converts new actions to events with correct `scheduling_event_id`
- Returns pending_actions for dispatcher

**is_replaying() support**:
- `is_replaying` field in `CtxInner` tracks replay vs new execution
- `ctx.is_replaying()` public method for orchestration code
- `persisted_history_len` in ReplayEngine tracks true DB history boundary
- `with_persisted_history_len()` builder method
- `HistoryManager.original_len()` returns DB event count

**Simplified trace API** (replay-guarded):
- `trace_simplified(level, msg)` - only emits when NOT replaying
- `trace_info_simplified()`, `trace_warn_simplified()`, `trace_error_simplified()`, `trace_debug_simplified()`
- No system calls or history events - just a guard on `is_replaying()`

**E2E tests passing** (5 tests):
- `simplified_single_activity` - Single activity workflow ✓
- `simplified_sequential_activities` - Two activities in sequence ✓
- `simplified_timer` - Timer-based workflow ✓
- `simplified_trace_emits_once` - Traces only emit once (not during replay) ✓
- `simplified_is_replaying_transitions` - is_replaying() correctness ✓
- Run: `cargo nt --test simplified_replay_e2e`

**Tests** - 529 passing (full suite):
- 8 Phase 1 harness tests
- 5 real `OrchestrationContext` tests  
- 11 replay engine tests
- 5 e2e simplified mode tests
- Run: `cargo nt`

---

## Next Steps

### Expand e2e coverage with more complex patterns

Add tests for:
1. Fan-out/fan-in (parallel activities with `ctx.join()`)
2. Sub-orchestrations
3. External events (`schedule_wait()` + raise_event)
4. Continue-as-new
5. Error handling (activity failure)
6. `ctx.select2()` / cancellation

---

## Implementation Checklist

| Step | Status | Notes |
|------|--------|-------|
| 1. Add `ReplayMode` + state fields to `CtxInner` | Done | |
| 2. Add `simplified_token` to `DurableFuture` | Done | |
| 3. Mode-aware `poll()` | Done | Only checks results, no action emission |
| 4. Mode-aware `schedule_*()` | Done | Emits actions at schedule-time |
| 5. Test: schedule order validation | Done | Actions emitted in schedule order |
| 6. Implement simplified `execute_orchestration()` | Done | Event-processing loop |
| 7. Feature flag for mode selection | Done | `RuntimeOptions.use_simplified_replay` |
| 8. Simple e2e tests with feature switch | Done | 5 tests passing |
| 9. `is_replaying()` support | Done | For replay-guarded side effects |
| 10. Simplified trace API | Done | `trace_*_simplified()` methods |
| 11. Complex e2e tests | **TODO** | Fan-out, sub-orch, external, CAN |

**Focused tests during development:**
- `cargo nt --test simplified_replay_e2e` - E2E simplified mode tests (5 tests)
- `cargo test replay_engine_simplified --lib` - All simplified replay tests (13 tests)
- `cargo test replay_engine::tests --lib` - ReplayEngine mode tests (2 tests)

**Full suite:**
- `cargo nt` - Everything (529 tests passing)

---

## Reference

### Key Files
- `src/lib.rs` - `CtxInner`, `ReplayMode`, `OrchestrationContext::schedule_*()`, `is_replaying()`, `trace_*_simplified()`
- `src/futures.rs` - `DurableFuture`, `poll()` implementation
- `src/runtime/mod.rs` - `RuntimeOptions.use_simplified_replay`
- `src/runtime/execution.rs` - `ReplayEngine` instantiation with mode flag and persisted_history_len
- `src/runtime/replay_engine.rs` - Production `ReplayEngine`, `execute_orchestration_simplified()`
- `src/runtime/replay_engine_simplified.rs` - Phase 1 harness + modal switch tests
- `src/runtime/state_helpers.rs` - `HistoryManager.original_len()`
- `tests/simplified_replay_e2e.rs` - E2E tests with simplified mode

---

## Change Log

| Date | Change |
|------|--------|
| 2026-01-19 | Phase 1 harness complete (8 tests) |
| 2026-01-19 | Modal switch infrastructure added |
| 2026-01-19 | Bug: actions emitted at poll-time, not schedule-time |
| 2026-01-19 | FIX: Actions now emitted at schedule-time (5 real ctx tests passing) |
| 2026-01-19 | `execute_orchestration_simplified()` implemented with event-processing loop |
| 2026-01-19 | `with_simplified_mode(true)` builder added to ReplayEngine |
| 2026-01-19 | `RuntimeOptions.use_simplified_replay` added for runtime-level toggle |
| 2026-01-19 | Bug: completion messages not delivered (skipped prep_completions) |
| 2026-01-19 | FIX: Store completion_messages, process in simplified loop |
| 2026-01-19 | Bug: action scheduling_event_id was token, not event_id |
| 2026-01-19 | FIX: `update_action_event_id()` corrects scheduling_event_id |
| 2026-01-19 | Bug: sequential activities failed - completions not persisted to history |
| 2026-01-19 | FIX: Add completion events to history_delta in simplified mode |
| 2026-01-19 | E2E tests passing: single, sequential, timer (3/3) |
| 2026-01-19 | Unified completion handling: prep_completions() runs for both modes |
| 2026-01-19 | Added `is_replaying` field and `ctx.is_replaying()` public API |
| 2026-01-19 | Added `persisted_history_len` to track true replay boundary |
| 2026-01-19 | Added simplified trace API: `trace_*_simplified()` (replay-guarded) |
| 2026-01-19 | Added 2 new tests: trace_emits_once, is_replaying_transitions |
| 2026-01-19 | Full suite passing: 529 tests |
