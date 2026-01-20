# Replay Simplification – Rolling Progress

**Spec:** [proposals/replay-simplification.md](replay-simplification.md)  
**Checkpoint commit:** `555c5751653ac770a94d3962f743b7a2d7f6ad58` (revert here if approach fails)

---

## Current State (2026-01-19 - Phase 2: Bring the hammer down)

### Phase 1 Complete ✓

All Phase 1 work is complete:
- Modal switch infrastructure
- `simplified_schedule_*()` APIs
- `execute_orchestration_simplified()` event-processing loop
- `is_replaying()` support
- Simplified trace API
- 30+ e2e tests passing with simplified mode

### Phase 2: Bring the hammer down

**Objective:** Complete migration to simplified replay engine and remove ALL legacy code.

#### Step 1: Port all tests to simplified_* APIs
- [ ] Port remaining e2e_samples.rs tests to simplified_replay_e2e.rs
- [ ] Port unit_tests.rs tests
- [ ] Port orchestration_stall_tests.rs tests
- [ ] Port cancellation_tests.rs tests
- [ ] Port all scenario tests
- [ ] Document skipped tests with reasons

#### Step 2: Remove legacy replay infrastructure

**To be removed from `src/lib.rs`:**
- [ ] `DurableFuture` struct and methods (`into_activity()`, `into_timer()`, `into_event()`, `into_sub_orchestration()`)
- [ ] Legacy `schedule_*()` methods (non-simplified versions)
- [ ] `run_turn()`, `run_turn_with()`, `run_turn_with_status()`, `run_turn_with_status_and_cancellations()`
- [ ] `poll_once()` function
- [ ] Legacy `TurnResult` type
- [ ] Legacy `CtxInner` fields: `history`, `next_event_id`, `claimed_scheduling_events`, `cursor`, etc.
- [ ] `ReplayMode` enum

**To be removed from `src/futures.rs`:**
- [ ] `DurableFuture` `Future` implementation (cursor/history scanning)
- [ ] `AggregateDurableFuture`, `SelectFuture`, `JoinFuture` (legacy versions)
- [ ] All cursor-based replay logic

**To be removed from `src/runtime/`:**
- [ ] `replay_engine.rs` legacy mode branch
- [ ] `replay_engine_simplified.rs` (test-only harness)
- [ ] `use_simplified_mode` flag

#### Step 3: Rename simplified_* to become THE API
- [ ] `simplified_schedule_activity()` → `schedule_activity()`
- [ ] `simplified_schedule_timer()` → `schedule_timer()`
- [ ] `simplified_schedule_wait()` → `schedule_wait()`
- [ ] `simplified_schedule_sub_orchestration()` → `schedule_sub_orchestration()`
- [ ] `simplified_join*()` → `join*()`
- [ ] `simplified_select*()` → `select*()`
- [ ] `trace_*_simplified()` → `trace_*()` or integrate
- [ ] Remove `use_simplified_replay` from `RuntimeOptions`

#### Step 4: Documentation update
- [ ] Update `docs/ORCHESTRATION-GUIDE.md`
- [ ] Update all examples
- [ ] Update internal docs

---

## Migration Report Template

### Tests Successfully Ported
| Test File | Test Name | Notes |
|-----------|-----------|-------|
| | | |

### Tests Skipped (with reasons)
| Test File | Test Name | Reason | Options to Fix |
|-----------|-----------|--------|----------------|
| | | | |

### Tests Disabled (no longer relevant)
| Test File | Test Name | Why Not Relevant |
|-----------|-----------|------------------|
| | | |

### Semantic Changes Discovered
| Area | Change | Impact |
|------|--------|--------|
| | | |

---

## Reference

### Key Files
- `src/lib.rs` - `OrchestrationContext`, simplified_* APIs
- `src/runtime/replay_engine.rs` - `execute_orchestration_simplified()`
- `tests/simplified_replay_e2e.rs` - E2E tests with simplified mode

### Commands
- `cargo nt --test simplified_replay_e2e` - Simplified mode tests
- `cargo nt` - Full test suite

---

## Change Log

| Date | Change |
|------|--------|
| 2026-01-19 | Phase 1 complete: modal switch, simplified APIs, 30+ tests |
| 2026-01-19 | Added `simplified_schedule_sub_orchestration_with_id()` for explicit child IDs |
| 2026-01-19 | Added `simplified_schedule_orchestration()` for fire-and-forget |
| 2026-01-19 | Phase 2 plan finalized: port tests, remove legacy, rename APIs |
