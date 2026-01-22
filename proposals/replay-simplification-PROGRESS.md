# Replay Simplification – Rolling Progress

**Spec:** [proposals/replay-simplification.md](replay-simplification.md)  
**Checkpoint commit:** `555c5751653ac770a94d3962f743b7a2d7f6ad58` (revert here if approach fails)

---

## Current State (2026-01-21 - ALL PHASES COMPLETE)

### 605 tests passing, 0 skipped ✓

All phases of replay simplification are complete. The codebase now uses simplified replay exclusively with clean APIs.

### Phase 2: Bring the hammer down - COMPLETE ✓

#### Step 1: Default to simplified mode ✓
- [x] Simplified replay is now the only mode
- [x] All runtime tests pass

#### Step 2: Remove legacy replay infrastructure ✓

**Removed from `src/lib.rs`:**
- [x] `run_turn()`, `run_turn_with()`, `run_turn_with_status()`, `run_turn_with_status_and_cancellations()` - REMOVED
- [x] Legacy `TurnResult` type - REMOVED  
- [x] `ReplayMode` enum - REMOVED (simplified is the only mode)
- [x] `set_replay_mode()` - REMOVED
- [x] `take_actions()` - REMOVED
- [x] `take_cancelled_activity_ids()` - REMOVED
- [x] `use_simplified_replay` option - REMOVED

**Removed from `src/runtime/replay_engine.rs`:**
- [x] Legacy mode branch in `execute_orchestration()` - REMOVED

**Removed from `src/futures.rs`:**
- [x] Legacy `DurableFuture::poll()` branches (~700 lines) - REMOVED
- [x] File now only contains `generate_guid()` utility

**Removed from `CtxInner`:**
- [x] Legacy fields (history, next_event_id, cursor, etc.) - REMOVED
- [x] Clean simplified state only

#### Step 3: Tests cleanup ✓
- [x] Legacy stall tests removed (used run_turn() API)
- [x] All remaining tests updated to use simplified APIs
- [x] 605 tests running, 0 skipped

### Phase 3: API Rename - COMPLETE ✓

All APIs now use clean names without `simplified_` prefix:

- [x] `schedule_activity()` - schedules activity, returns future
- [x] `schedule_timer()` - schedules timer, returns future
- [x] `schedule_wait()` - schedules external event wait, returns future
- [x] `schedule_sub_orchestration()` - schedules sub-orchestration, returns future
- [x] `schedule_sub_orchestration_with_id()` - with explicit child ID
- [x] `schedule_orchestration()` - fire-and-forget detached orchestration
- [x] `join()`, `join2()`, `join3()` - deterministic join combinators
- [x] `select2()`, `select3()` - deterministic select combinators

### Phase 4: Documentation - TODO

- [ ] Update `docs/ORCHESTRATION-GUIDE.md` (review for accuracy)
- [ ] Update `docs/durable-futures-internals.md` (major rewrite needed)
- [ ] Update `docs/replay-engine.md` (if exists)
- [ ] Review examples in `examples/` (likely already correct)

---

## Migration Summary

### Final State
- **605 tests passing**, 0 skipped
- **0 compiler warnings** (build)
- **2 clippy doc warnings** (missing `# Errors` sections - minor)
- Clean API surface with no legacy code

### Key Behavioral Notes
| Area | Behavior |
|------|----------|
| `join()` | Returns results in schedule order (not history completion order) |
| `select2/3()` | Uses `futures::select_biased!` for deterministic winner selection |
| `trace*()` | Traces are emitted but not stored in history |

### TBD: Cancellation Wiring (Orchestration → Activities)

During review, the following cancellation semantics appear incomplete and should be addressed (or explicitly documented as intentional):

- **Orchestration cancellation should cancel in-flight activities**
	- Today, terminal states **Completed/Failed/ContinuedAsNew** compute in-flight activities and pass them to provider lock-stealing via `cancelled_activities`.
	- However, `TurnResult::Cancelled` does **not** currently trigger the same in-flight activity cancellation; it only appends a cancelled failure and propagates cancellation to child sub-orchestrations.
	- **TBD:** include `Cancelled` in the same “cancel all in-flight activities” behavior used for other terminal states.

- **Select-loser activity cancellation is plumbed but not produced**
	- `ReplayEngine.cancelled_activity_ids` exists and the runtime collects it, but the replay engine does not appear to populate it.
	- **TBD:** implement loser tracking in `select2/3()` (or elsewhere) such that losing activities can be lock-stolen when the winner completes.

- **Clarify cancellation model vs proposal**
	- The proposal mentions dehydration + Drop-based cancellation for physical activity cancellation.
	- The current simplified schedule futures are `poll_fn`-based and do not implement Drop-driven cancellation.
	- **TBD:** either implement dehydration + Drop-driven cancellation, or document that orchestration-level Drop does not trigger physical cancellation (and rely solely on terminal-state lock stealing).

---

## Commands
- `cargo nt` - Full test suite (605 tests)
- `cargo nt -E 'test(/replay_engine::/)' ` - Replay engine tests (113 tests)
- `cargo clippy --lib --all-features` - Check for warnings

---

## Change Log

| Date | Change |
|------|--------|
| 2026-01-21 | Phase 3 complete: API rename done, all legacy code removed |
| 2026-01-21 | Added 113 replay_engine unit tests with orchestration code documentation |
| 2026-01-20 | Phase 2 complete: simplified mode default, legacy removed |
| 2026-01-19 | Phase 1 complete: modal switch, simplified APIs |
