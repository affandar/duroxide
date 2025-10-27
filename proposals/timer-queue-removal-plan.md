# Proposal: Remove Timer Queue by Using Orchestrator Queue Delayed Visibility

## Current Timer Flow

1. **Orchestration schedules timer** → `ctx.schedule_timer(delay_ms)` creates `Action::CreateTimer`
2. **Runtime generates WorkItem** → `run_single_execution_atomic()` creates `WorkItem::TimerSchedule` with `fire_at_ms`
3. **Enqueue to timer queue** → `ack_orchestration_item()` enqueues `timer_items` to dedicated `timer_queue` table
4. **Timer dispatcher polls** → Separate dispatcher calls `dequeue_timer_peek_lock()` 
5. **Convert and enqueue** → When `fire_at <= now`, dispatcher calls `ack_timer()` which:
   - Deletes `TimerSchedule` from timer_queue
   - Enqueues `TimerFired` to orchestrator_queue

## Proposed New Flow

1. **Orchestration schedules timer** → `ctx.schedule_timer(delay_ms)` creates `Action::CreateTimer` (unchanged)
2. **Runtime generates both events atomically** → `run_single_execution_atomic()`:
   - Adds `Event::TimerScheduled` to history (for deterministic replay)
   - Creates `WorkItem::TimerFired` in orchestrator_items (for delayed execution)
3. **Enqueue to orchestrator queue with delay** → `ack_orchestration_item()` enqueues `TimerFired` to orchestrator_queue with `visible_at = fire_at_ms`
4. **Orchestrator dispatcher handles naturally** → When `visible_at <= now`, orchestrator dispatcher dequeues and processes `TimerFired`

## Key Insight

The critical improvement is to **enqueue both `TimerScheduled` (to history) and `TimerFired` (to orchestrator queue) in the same transaction**. This preserves:

- **Complete history** for deterministic replay
- **Minimal changes** to orchestrator code (schedule_timer() works the same)
- **Atomic commitment** of both the scheduling event and the delayed work item

## Key Benefits

1. **Simpler Architecture** - Eliminates entire timer queue and dispatcher
2. **Leverages Existing Infrastructure** - Orchestrator queue already has delayed visibility support
3. **More Atomic** - Timer "firing" is just visibility threshold, no state transition needed
4. **Fewer Moving Parts** - One less queue, one less dispatcher, three less Provider methods
5. **Preserves Determinism** - TimerScheduled in history means replay works identically

## Changes Required

### 1. Runtime Execution (`src/runtime/execution.rs`)

**File**: `src/runtime/execution.rs` line 167-179

```rust
// BEFORE:
crate::Action::CreateTimer {
    scheduling_event_id,
    delay_ms,
} => {
    let execution_id = self.get_execution_id_for_instance(instance).await;
    let fire_at_ms = Self::calculate_timer_fire_time(&turn.final_history(), *delay_ms);
    timer_items.push(WorkItem::TimerSchedule {
        instance: instance.to_string(),
        execution_id,
        id: *scheduling_event_id,
        fire_at_ms,
    });
}

// AFTER:
crate::Action::CreateTimer {
    scheduling_event_id,
    delay_ms,
} => {
    let execution_id = self.get_execution_id_for_instance(instance).await;
    let fire_at_ms = Self::calculate_timer_fire_time(&turn.final_history(), *delay_ms);
    
    // Record TimerScheduled in history for deterministic replay
    history_delta.push(Event::TimerScheduled {
        event_id: *scheduling_event_id,
        fire_at_ms,
    });
    
    // Enqueue TimerFired to orchestrator queue with delayed visibility
    // Provider will use fire_at_ms for the visible_at timestamp
    orchestrator_items.push(WorkItem::TimerFired {
        instance: instance.to_string(),
        execution_id,
        id: *scheduling_event_id,
        fire_at_ms,
    });
}
```

**Impact**: 
- Remove all references to `timer_items` vector
- Add `TimerScheduled` to history_delta
- Add `TimerFired` to orchestrator_items
- Timer becomes just another orchestrator message with delayed visibility
- Replay engine sees `TimerScheduled` in history and knows to expect `TimerFired`

### 2. Provider Trait (`src/providers/mod.rs`)

**Remove these methods**:
- `async fn enqueue_timer_work(&self, _item: WorkItem) -> Result<(), String>`
- `async fn dequeue_timer_peek_lock(&self) -> Option<(WorkItem, String)>`  
- `async fn ack_timer(&self, token: &str, completion: WorkItem, delay_ms: Option<u64>) -> Result<(), String>`

**Update `ack_orchestration_item` signature**:
```rust
// BEFORE: 
async fn ack_orchestration_item(
    &self,
    _lock_token: &str,
    _execution_id: u64,
    _history_delta: Vec<Event>,
    _worker_items: Vec<WorkItem>,
    _timer_items: Vec<WorkItem>,      // REMOVE THIS
    _orchestrator_items: Vec<WorkItem>,
    _metadata: ExecutionMetadata,
) -> Result<(), String>;

// AFTER:
async fn ack_orchestration_item(
    &self,
    _lock_token: &str,
    _execution_id: u64,
    _history_delta: Vec<Event>,
    _worker_items: Vec<WorkItem>,
    _orchestrator_items: Vec<WorkItem>,  // Now includes TimerFired with delayed visibility
    _metadata: ExecutionMetadata,
) -> Result<(), String>;
```

**Update enqueue_orchestrator_work documentation**:
- Note that it can receive `TimerFired` items
- These items should use the `fire_at_ms` field for the `visible_at` column
- Document that `visible_at` controls when the item becomes visible to the dispatcher

### 3. SQLite Provider (`src/providers/sqlite.rs`)

**Schema Changes**:
- **Remove**: `timer_queue` table creation
- **Remove**: `idx_timer_fire` index

**Remove implementations**:
- `enqueue_timer_work()`
- `dequeue_timer_peek_lock()`
- `ack_timer()`

**Update `ack_orchestration_item()`**:
```rust
// When enqueuing orchestrator_items, check if it's TimerFired
// and use fire_at_ms for visible_at

for item in orchestrator_items {
    let work_item = serde_json::to_string(&item).map_err(|e| e.to_string())?;
    let instance = match &item {
        WorkItem::StartOrchestration { instance, .. } => instance,
        WorkItem::TimerFired { instance, .. } => instance,
        WorkItem::ActivityCompleted { instance, .. } => instance,
        WorkItem::ActivityFailed { instance, .. } => instance,
        WorkItem::EventRaised { instance, .. } => instance,
        // ... other variants
    };
    
    // Set visible_at based on item type
    // TimerFired uses fire_at_ms to delay visibility until timer should fire
    let visible_at = match &item {
        WorkItem::TimerFired { fire_at_ms, .. } => *fire_at_ms as i64,
        _ => Self::now_millis(),
    };
    
    sqlx::query("INSERT INTO orchestrator_queue (instance_id, work_item, visible_at) VALUES (?, ?, ?)")
        .bind(instance)
        .bind(work_item)
        .bind(visible_at)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
}
```

**Update `ack_orchestration_item` signature**:
- Remove `timer_items: Vec<WorkItem>` parameter

### 4. Runtime Module (`src/runtime/mod.rs`)

**Remove**:
- `start_timer_dispatcher()` function entirely
- Call to `start_timer_dispatcher()` in `start_with_options()`
- Timer dispatcher task registration (lines ~274-277)

**Update all calls to `ack_orchestration_item`**:
- Remove `timer_items` argument
- Only pass `orchestrator_items` (which now includes `TimerFired`)

**Update `handle_orchestration_atomic()`**:
- Remove `timer_items` from return type
- Change return from `(Vec<WorkItem>, Vec<WorkItem>, Vec<WorkItem>)` to `(Vec<WorkItem>, Vec<WorkItem>)`
- Only return `(worker_items, orchestrator_items)`

**Update `ack_orchestration_with_changes()`**:
- Remove `timer_items` parameter
- Only handle `worker_items` and `orchestrator_items`

### 5. Test Updates

**Tests to update** (remove `timer_items` arguments):
- `tests/sqlite_tests.rs` - All `ack_orchestration_item()` calls
- `tests/provider_atomic_tests.rs` - All `ack_orchestration_item()` calls  
- `tests/common/mod.rs` - Test helper functions
- `examples/sqlite_persistence.rs` - Example code

**Timer-specific test updates**:
- `tests/timer_tests.rs::test_timer_queue_operations` - Rewrite to verify orchestrator queue behavior
- Replace `enqueue_timer_work()` with `enqueue_orchestrator_work()` 
- Replace `dequeue_timer_peek_lock()` with `fetch_orchestration_item()`
- Verify `visible_at` timestamps are respected
- Verify `TimerScheduled` appears in history
- Verify `TimerFired` is only visible after `fire_at_ms`

### 6. Documentation Updates

**Files to update**:
- `docs/provider-implementation-guide.md` - Remove timer queue section, update orchestrator queue to note it handles timers with delayed visibility
- `docs/polling-and-execution-model.md` - Update dispatcher architecture (remove timer dispatcher)
- `docs/sqlite-provider-design.md` - Remove timer queue schema, document `visible_at` for timers
- `docs/postgresql-provider-plan.md` - Update future provider plan (no timer queue needed)
- `docs/architecture.md` - Update architectural diagrams to show single dispatcher model

## Why This Approach Preserves Determinism

When an orchestration replays:

1. **During initial execution**:
   - Orchestrator calls `schedule_timer(5000)`
   - Runtime records `Event::TimerScheduled { event_id: 3, fire_at_ms: 12345 }` in history
   - Runtime enqueues `WorkItem::TimerFired { id: 3, fire_at_ms: 12345 }` with delayed visibility
   - Orchestration waits for timer (suspended)

2. **Timer fires (later)**:
   - Orchestrator dispatcher dequeues `TimerFired` (now visible)
   - Runtime replays history, sees `TimerScheduled` at event_id 3
   - Runtime delivers `TimerFired` event to orchestrator
   - Orchestrator receives timer and continues

3. **During replay (after recovery)**:
   - Runtime loads history from persistence
   - Runtime sees `Event::TimerScheduled { event_id: 3, fire_at_ms: 12345 }` in history
   - Replay engine knows timer was scheduled and matches it with incoming `TimerFired`
   - Orchestration proceeds deterministically

The key: **`TimerScheduled` in history ensures the replay engine knows a timer was created**, so it can properly match the eventual `TimerFired` work item.

## Migration Path (If Needed)

For existing databases with timer_queue data:

```sql
-- Migrate pending timers from timer_queue to orchestrator_queue
-- Convert WorkItem::TimerSchedule to WorkItem::TimerFired
INSERT INTO orchestrator_queue (instance_id, work_item, visible_at)
SELECT 
    json_extract(work_item, '$.TimerSchedule.instance'),
    json_set(
        work_item,
        '$.TimerFired', json_extract(work_item, '$.TimerSchedule')
    ),
    fire_at
FROM timer_queue
WHERE lock_token IS NULL OR locked_until <= current_timestamp;

-- Drop timer_queue table
DROP TABLE timer_queue;

-- Note: This migration is safe because:
-- 1. TimerScheduled events already exist in history from original execution
-- 2. We're just moving pending TimerSchedule -> TimerFired conversions
-- 3. The replay engine will match TimerFired with existing TimerScheduled history
```

## Validation

After changes, verify:

1. ✅ All timer tests pass (`cargo test --test timer_tests`)
2. ✅ Orchestrator queue correctly respects `visible_at` for `TimerFired` items
3. ✅ `TimerScheduled` events appear in history when timers are created
4. ✅ Replay correctly handles timer scheduling and firing
5. ✅ No timer dispatcher running (`ps aux | grep timer`)
6. ✅ `timer_queue` table not created in new databases
7. ✅ Provider trait no longer has timer-specific methods
8. ✅ Performance unchanged or improved (fewer dispatchers = less overhead)

## Risks and Considerations

1. **Backward Compatibility**: Existing databases with pending timers in `timer_queue` won't automatically migrate
   - Mitigation: Provide migration SQL or add automatic migration in `create_schema()`
   - Note: History already has `TimerScheduled` events, so replay is safe
   
2. **Orchestrator Queue Load**: All timers now flow through orchestrator queue
   - Analysis: This is actually better - orchestrator queue already handles all orchestration messages
   - No additional load since timers were always going there eventually
   - Benefit: One less queue to poll means better CPU efficiency
   
3. **Visibility Granularity**: Orchestrator queue `visible_at` must support millisecond precision
   - Current: Already using `TIMESTAMP` which supports this
   - No changes needed
   
4. **Testing Coverage**: Need to ensure timer visibility behavior is well-tested
   - Add specific tests for `visible_at` filtering in orchestrator queue
   - Test that `TimerScheduled` appears in history
   - Test that `TimerFired` respects visibility delay

5. **History Size**: `TimerScheduled` events now stored in history
   - Analysis: These events were already being stored (as part of the full event log)
   - No increase in storage requirements
   - Benefit: More complete audit trail

## Estimated Impact

- **Lines Removed**: ~250-350 (timer queue methods, dispatcher, table schema, tests)
- **Lines Added**: ~30-50 (updated orchestrator enqueue logic, history event)
- **Net Reduction**: ~200-300 lines
- **Complexity Reduction**: Significant - removes entire subsystem
- **Provider Interface**: Simpler - 3 fewer methods to implement
- **Architectural Clarity**: Single dispatcher model is easier to understand

## Conclusion

This change significantly simplifies the architecture by recognizing that timers are just delayed orchestrator messages. The orchestrator queue already has all the infrastructure needed (delayed visibility), so we can eliminate the redundant timer-specific queue and dispatcher entirely.

**The key insight**: A timer "firing" is simply a message becoming visible in the queue at the right time, not a separate state transition requiring a dedicated queue.

**The critical improvement**: By enqueuing both `TimerScheduled` (to history) and `TimerFired` (to orchestrator queue) in the same transaction, we preserve complete deterministic replay while eliminating the timer queue infrastructure. The `schedule_timer()` method and replay engine continue to work unchanged, making this a low-risk, high-value simplification.

## Implementation Order

1. **Update runtime execution** to push both history event and work item
2. **Update provider trait** to remove timer methods and `timer_items` parameter
3. **Update SQLite provider** to handle `TimerFired` with `visible_at` in orchestrator queue
4. **Remove timer dispatcher** from runtime module
5. **Update tests** to verify new behavior
6. **Update documentation** to reflect single-dispatcher architecture
7. **Provide migration path** for existing databases (if needed)
