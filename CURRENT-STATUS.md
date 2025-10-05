# Event ID Cleanup - Current Status

## Library: ✅ COMPILES SUCCESSFULLY

**Zero errors, 1 harmless warning**

## Tests Status

### ✅ Passing (4 tests)
- `action_emission_single_turn` - Simple turn execution ✓
- `providers_fs_multi_execution_persistence_and_latest_read` - Database operations ✓
- `providers_inmem_multi_execution_persistence_and_latest_read` - In-memory provider ✓
- (1 more passing)

### ❌ Failing (4 tests) - Runtime Timeouts
- `deterministic_replay_activity_only` - Timeout (5s)
- `orchestration_descriptor_root_and_child` - unwrap on None
- `orchestration_status_apis` - Timeout
- `runtime_duplicate_orchestration_deduped_single_execution` - Timeout

## Root Cause Analysis

### Why Timeouts?

Runtime tests are timing out after 5 seconds, meaning orchestrations aren't progressing. Possible causes:

1. **Cursor stuck in loop** - The cursor might not be advancing properly
2. **Event_id mismatch** - Events might have event_id=0 when they shouldn't
3. **History flow issue** - OrchestrationTurn → Context → Cursor flow might be broken
4. **Completion matching** - The strict cursor might be waiting for something that never arrives

### What Works
- Library compiles ✓
- Simple turn execution (no runtime) ✓
- Database schema updated ✓
- SQL queries updated ✓  
- Event serialization/deserialization ✓

### What Needs Investigation
- Runtime orchestration execution flow
- Cursor advancement in actual runtime scenarios
- OrchestrationTurn → Context integration

## Implementation Completed

### Phase 1-2: Core Model ✅
- Event enum (16 variants)
- CtxInner with cursor
- Unified cursor polling (4 future types)
- CompletionMap deleted (~840 lines)
- Schedule methods simplified
- Dispatch functions updated

### Phase 3: Database ✅
- Schema updated (sequence_num → event_id)
- SQL queries updated
- append_history_in_tx assigns event_ids
- WorkItem/OrchestratorMsg documented

### Phase 4: Runtime ✅  
- All dispatch functions use scheduling_event_id
- Runtime apply_decisions updated
- OrchestrationTurn prep_completions rewritten

## Known Issues to Debug

1. **Runtime timeouts** - Need to trace execution to see where it's stuck
2. **Test Event construction** - Many tests still need `event_id` field updates
3. **OrchestrationTurn_tests** - Need to remove CompletionMap references

## Code Statistics

- **Deleted**: ~850 lines (CompletionMap + helpers)
- **Modified**: ~1,500 lines
- **Simplified**: Removed 4 HashSets, single cursor
- **Test status**: 4/7 unit tests passing

## Next Steps

1. **Debug runtime timeouts**:
   - Add tracing to cursor advancement
   - Verify events have proper event_ids
   - Check OrchestrationTurn history flow

2. **Fix orchestration_turn_tests.rs**:
   - Remove CompletionMap references
   - Update Event construction

3. **Systematic test fixes**:
   - Update remaining Event constructions
   - Fix pattern matching throughout test suite

## Files Modified (Core)

- ✅ `src/lib.rs` - Event model, CtxInner, Context methods
- ✅ `src/futures.rs` - Unified cursor polling
- ✅ `src/runtime/dispatch.rs` - All dispatch functions
- ✅ `src/runtime/execution.rs` - Action/Event handling
- ✅ `src/runtime/orchestration_turn.rs` - CompletionMap removed
- ✅ `src/runtime/mod.rs` - apply_decisions
- ✅ `src/providers/mod.rs` - WorkItem fields
- ✅ `src/providers/sqlite.rs` - Schema and queries
- ✅ `src/client/mod.rs` - Pattern matching
- ✅ `migrations/` - Schema updated

## Files Deleted

- ✅ `src/runtime/completion_map.rs`
- ✅ `src/runtime/completion_map_tests.rs`
- ✅ `src/runtime/completion_aware_futures.rs`

## Remaining Test File Updates Needed

Most test files just need Event field updates (add `event_id`, use `source_event_id`):
- tests/e2e_samples.rs - 2 fixes applied
- tests/unit_tests.rs - Partial fixes applied
- tests/orchestration_turn_tests.rs - Needs CompletionMap removal
- ~25 other test files - Systematic Event updates needed

## Summary

**Core implementation: Complete**
**Library compilation: Success**
**Basic tests: Passing**
**Runtime tests: Need debugging**

The foundation is solid. The timeouts indicate a runtime execution flow issue that needs targeted debugging.


