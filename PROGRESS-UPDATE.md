# Event ID Cleanup - Progress Update

## ✅ COMPLETED (Phases 1 & 2.1-2.2)

### Core Data Model Changes
- ✅ Event enum: All variants have `event_id`, removed `id`, added `source_event_id`
- ✅ Event helpers: `event_id()`, `set_event_id()`, `source_event_id()`
- ✅ Action enum: All use `scheduling_event_id`
- ✅ CtxInner: Added cursor, removed all HashSets (~50 lines simpler!)
- ✅ DurableFuture Kind: Updated with `claimed_event_id`, removed `id` and `scheduled`

### Unified Cursor Implementation
- ✅ Activity polling: Complete cursor-based implementation with strict validation
- ✅ Timer polling: Complete cursor-based implementation
- ✅ External polling: Complete with special name-based matching
- ✅ SubOrch polling: Complete cursor-based implementation
- ✅ AggregateDurableFuture: Simplified (no longer needs event_ids upfront)

### Schedule Methods Simplified
- ✅ `schedule_activity()`: No ID allocation, just creates future
- ✅ `schedule_timer()`: No ID allocation
- ✅ `schedule_wait()`: No ID allocation
- ✅ `schedule_sub_orchestration()`: No ID allocation
- ✅ `schedule_orchestration()`: Uses next_event_id directly

### Code Deleted
- ✅ `src/runtime/completion_map.rs` (~280 lines)
- ✅ `src/runtime/completion_map_tests.rs` (~300 lines)
- ✅ `src/runtime/completion_aware_futures.rs` (~260 lines)
- ✅ Removed `try_completion_map_poll()` from DurableFuture
- ✅ Removed module declarations from runtime/mod.rs

**Total deleted: ~850 lines!**

## 🔄 REMAINING WORK

### Critical Compilation Fixes Needed

1. **`src/runtime/dispatch.rs`** - Fix Event pattern matching:
   - Change `Event::ActivityCompleted { id: cid, .. }` → `{ source_event_id, .. }`
   - Change `Event::TimerCreated { id, .. }` → `{ event_id, .. }`
   - Change `Event::TimerFired { id, .. }` → `{ source_event_id, .. }`
   - Update function signatures to use `scheduling_event_id`

2. **`src/runtime/execution.rs`** - Event construction:
   - All Event construction needs `event_id: 0` and `source_event_id` where applicable
   - Action construction needs `scheduling_event_id`

3. **`src/runtime/orchestration_turn.rs`** - Remove CompletionMap usage:
   - Remove `completion_map` field from OrchestrationTurn
   - Update `prep_completions()` to convert messages → events directly
   - Remove completion_map methods
   - Update imports

4. **`src/runtime/orchestration_turn_tests.rs`** - Remove CompletionMap imports

5. **`src/lib.rs`** - Remove old helper methods:
   - `find_history_index()` - no longer needed
   - `synth_output_from_history()` - no longer needed
   - `claimed_ids_snapshot()` - no longer needed

6. **`src/providers/mod.rs`** - Update WorkItem:
   - `ActivityExecute`: `id` → `scheduling_event_id`
   - `ActivityCompleted`: `id` → `source_event_id`
   - `TimerSchedule`: `id` → `scheduling_event_id`
   - `TimerFired`: `id` → `source_event_id`
   - `SubOrchCompleted/Failed`: `parent_id` → `source_event_id`

7. **`src/runtime/router.rs`** - Update OrchestratorMsg:
   - Similar changes to WorkItem

8. **Database schema** (`migrations/`) - Rename `sequence_num` → `event_id`

9. **Provider SQL queries** - Update all queries

10. **All test files** (~30 files) - Update Event construction

### Estimated Time Remaining

- Runtime fixes (dispatch, execution, turn): 2-3 hours
- WorkItem/OrchestratorMsg updates: 1 hour  
- Provider/database updates: 1-2 hours
- Test updates: 3-4 hours (many files!)

**Total**: ~7-10 hours of systematic fixes

## Current Build Status

**Will NOT compile** - Expected!

Errors remaining:
- Event pattern matching (dispatch.rs, execution.rs, etc.)
- WorkItem field names
- OrchestratorMsg field names
- Test files (~30 files)
- Provider SQL queries

## What's Working

The **core algorithm** is implemented:
- Single cursor advances through history
- Strict validation (next event MUST match)
- No searching for completions
- Clean separation of scheduling vs completion

This is the hardest part - the rest is mechanical find-and-replace!

## Next Steps for Implementation Session

1. Fix runtime/dispatch.rs
2. Fix runtime/execution.rs  
3. Update OrchestrationTurn
4. Update WorkItem/OrchestratorMsg
5. Run cargo fix for automatic updates
6. Manually fix remaining test files
7. Update database schema
8. Final testing

The design is solid and the core is implemented! 🎯


