# Implementation Checkpoint - Event ID Cleanup

## Status: Phase 1 Complete ✓

### Completed Changes

✅ **Event enum updated** (`src/lib.rs:240-347`)
- All variants now have `event_id: u64`
- Scheduling events: `id` removed, `event_id` is THE id
- Completion events: Added `source_event_id: u64` (except ExternalEvent)
- Clean separation of concerns

✅ **Event helper methods** (`src/lib.rs:349-407`)
- `event.event_id()` - Get event_id from any event
- `event.set_event_id(id)` - Set event_id (used by runtime)
- `event.source_event_id()` - Get source_event_id for completions

✅ **Action enum updated** (`src/lib.rs:420-458`)
- All actions now use `scheduling_event_id` instead of `id`
- Clear naming: references the scheduling event's event_id

✅ **CtxInner simplified** (`src/lib.rs:460-497`)
- Added: `next_event_id: u64` - for creating new events
- Added: `next_event_index: usize` - single unified cursor
- Removed: `next_correlation_id` - no longer needed
- Removed: `claimed_activity_ids`, `claimed_timer_ids`, `claimed_external_ids` - cursor tracks this
- Removed: `next_id()` method - use next_event_id directly

✅ **DurableFuture Kind updated** (`src/futures.rs:134-159`)
- All variants now have `claimed_event_id: Cell<Option<u64>>`
- Removed: `id: u64` field - discovered lazily
- Removed: `scheduled: Cell<bool>` - not needed with new model

✅ **Completion map polling removed** (`src/futures.rs:18`)
- Removed `try_completion_map_poll()` method
- Ready for unified cursor implementation

## Next Steps (Phase 2)

### In Progress
🔄 **Phase 2.1**: Implement unified cursor polling in DurableFuture  
   - Replace `Future::poll()` implementation with cursor-based approach
   - Implement for all Kind variants (Activity, Timer, External, SubOrch)
   - Add strict validation and corruption checks

### Upcoming
- **Phase 2.2**: Delete CompletionMap files
- **Phase 2.3**: Update OrchestrationTurn
- **Phase 3**: Update WorkItem, OrchestratorMsg, database schema
- **Phase 4**: Update dispatch functions
- **Phase 5**: Update all tests
- **Phase 6**: Final cleanup

## Current State

The codebase will NOT compile right now due to:
1. References to old `id` field in Event pattern matching throughout codebase
2. DurableFuture::poll() still uses old logic (needs rewrite)
3. Tests use old Event structure
4. CompletionMap still referenced in runtime code

This is expected - we're in the middle of a large breaking change.

## Compilation Status

Expected errors:
- Event pattern matching needs `event_id` instead of `id`
- Event construction needs `event_id: 0` and `source_event_id`
- DurableFuture polling logic incompatible with new Kind structure
- Action construction needs `scheduling_event_id`

## Design Decisions Summary

1. **Single cursor** (`next_event_index`) for ALL events
2. **No HashSets** - cursor position tracks what's processed
3. **Strict validation** - next event of expected type MUST match
4. **Corruption checks** - validate name/input when claiming/consuming
5. **No searching** - forward-only cursor, no skipping completions

## Estimated Remaining Work

- Phase 2: 2 days (cursor implementation + CompletionMap removal)
- Phase 3: 1 day (WorkItem/database updates)
- Phase 4-6: 4 days (dispatch + tests + cleanup)

**Total remaining**: ~7 days

## Benefits Achieved So Far

- Event model is now consistent
- CtxInner is dramatically simpler
- Foundation for cursor-based determinism is in place
- ~200 lines of complexity already removed from CtxInner

## Next Session Plan

1. Implement unified cursor polling for Activity futures
2. Extend to Timer, External, SubOrch futures
3. Delete CompletionMap files
4. Update schedule methods in OrchestrationContext
5. Fix compilation errors in core files

This is excellent progress! The hardest design decisions are behind us, now it's systematic implementation.


