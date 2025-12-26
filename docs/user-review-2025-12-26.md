# Code Review Notes - December 26, 2025

## 1. `OrchestrationContinuedAsNew` Missing Name/Version

**File:** [src/lib.rs#L778](../src/lib.rs#L778)

**Comment:**
> This should have the name, version as well, otherwise there is no orch name version available in the current execution!!

**Current Code:**
```rust
OrchestrationContinuedAsNew { input: String },
```

**Response:** ✅ Agreed, this is a valid gap. When the new execution starts, it will have a fresh `OrchestrationStarted` event with name/version. However, for the *current* (ending) execution's history, we lose track of what orchestration was being continued. This matters for:
- Audit/observability (which orch continued-as-new?)
- Debugging replay issues
- Correlating the CAN event back to its source

**Action Plan:**
```rust
OrchestrationContinuedAsNew { 
    input: String,
    name: String,
    version: String,
}
```

---

## 2. Missing History Treated as Terminal

**File:** [src/runtime/dispatchers/orchestration.rs#L239-240](../src/runtime/dispatchers/orchestration.rs#L239-L240)

**Comment:**
> Missing history should be treated as terminal as well. Have a single terminal state? or is it just !Running

**Response:** ✅ Good call. The activity-cancellation proposal already adopts this pattern: `status != "Running"` is terminal. For orchestration dispatch:
- Missing/empty history without an `OrchestrationStarted` in the incoming batch *could* indicate corruption or a deleted instance
- Treating it as terminal is safer than crashing or looping

**Action Plan:**
Consider a helper like:
```rust
fn is_terminal_or_missing(&self) -> bool {
    self.is_completed || self.is_failed || self.is_continued_as_new || self.is_empty()
}
```

For the "single terminal state" question: Keep the distinction (`Completed`, `Failed`, `ContinuedAsNew`) for observability/metrics, but have a unified `is_terminal()` predicate.

---

## 3. When Would `has_orchestration_name()` Return False?

**File:** [src/runtime/dispatchers/orchestration.rs#L276](../src/runtime/dispatchers/orchestration.rs#L276)

**Comment:**
> Evaluate the conditions when this would return false

**Response:** This returns `false` when:
1. **Empty batch** – no messages in `completion_messages` and no `OrchestrationStarted` in history
2. **Orphan messages** – completion messages without a corresponding start event (corruption/race)
3. **Legacy/malformed data** – if a work item somehow got enqueued without an orchestration name

The current code handles this by logging a warning ("empty effective batch") and skipping. This is defensive and correct.

**Action Plan:** Add a metric counter for these cases to detect provider issues.

---

## 4. History Corruption Tests

**File:** [src/runtime/dispatchers/orchestration.rs#L310-311](../src/runtime/dispatchers/orchestration.rs#L310-L311)

**Comment:**
> We need to add history corruption tests. Duroxide needs to be able to handle malformed histories gracefully. This is like fuzz testing.

**Response:** ✅ Strongly agree. This is critical for production robustness.

**Action Plan:** Add test scenarios for:
- Missing `OrchestrationStarted` with completion messages present
- Duplicate event IDs
- Out-of-order event IDs
- Activity/timer completions referencing non-existent scheduled IDs
- Truncated JSON in event payloads
- Terminal event followed by more events
- Invalid event kind combinations

Consider using `proptest` or `quickcheck` for generative fuzzing. The runtime should either:
- Fail gracefully with a clear error (mark as poison)
- Self-heal where possible (skip invalid events)

---

## 5. Why Are Metadata Values `Option<>`?

**File:** [src/runtime/dispatchers/orchestration.rs#L340](../src/runtime/dispatchers/orchestration.rs#L340)

**Comment:**
> Evaluate this closely. There shouldn't be missing data, the `Some<>` is throwing me off. Analyze why would some of these values be none.

**Response:** The `Option<>` exists because `compute_execution_metadata` only sets these fields if it finds terminal events in the delta:
- **`status`** – only set if `OrchestrationCompleted`, `OrchestrationFailed`, or `OrchestrationContinuedAsNew` is in the delta
- **`output`** – same
- **`orchestration_name/version`** – only set if `OrchestrationStarted` is in the delta

For a non-terminal turn (still `Running`), these remain `None`. This is correct behavior.

**Action Plan:** Either:
1. Add a comment explaining the intentional `None` for in-progress orchestrations
2. Refactor to return an enum: `TurnOutcome::InProgress` vs `TurnOutcome::Terminal { status, output }`

---

## 6. Don't Scan History Twice

**File:** [src/runtime/mod.rs#L473](../src/runtime/mod.rs#L473)

**Comment:**
> Generate this metadata while processing the history, don't scan the history yet again here.

**Response:** ✅ Valid optimization. The history delta is already being iterated during `HistoryManager::extend()`.

**Action Plan:**
1. Have `HistoryManager::extend()` return metadata about what was appended
2. Or have the replay engine track terminal status as it emits events

This would eliminate the O(n) re-scan in `compute_execution_metadata`. Low priority unless history deltas get large.

---

## 7. Rename `turn`/`ReplayEngine` Refactor

**File:** [src/runtime/execution.rs#L81-85](../src/runtime/execution.rs#L81-L85)

**Comment:**
> Rename turn to replay_engine, and execute_orchestration fn to just replay(), TurnResult to ReplayResult. Also see if we can make the ReplayEngine completely stateless. i.e. we don't even need a struct. Just have a replay fn in the replay_engine.rs file. No need to load up the struct with all of these prep completion etc. We can just pass the messages to the replay fn and have it do the prep completions inside. Give a proposal to refactor this.

**Response:** ✅ Good refactor idea. Benefits:
- Clearer naming (`replay()` is more descriptive than `execute_orchestration()`)
- Stateless function = easier to test, no struct setup

**Action Plan - Proposed Signature:**
```rust
pub fn replay(
    instance: &str,
    execution_id: u64,
    history: &[Event],
    completion_messages: &[WorkItem],
    handler: Arc<dyn OrchestrationHandler>,
    input: String,
    orch_name: Option<String>,
    orch_version: Option<String>,
    worker_id: &str,
) -> ReplayResult { ... }
```

The struct is currently used to accumulate `pending_actions` and `history_delta`. A stateless version would return these as part of `ReplayResult`:
```rust
pub struct ReplayResult {
    pub outcome: ReplayOutcome, // Continue, Completed, Failed, ContinuedAsNew
    pub history_delta: Vec<Event>,
    pub pending_actions: Vec<PendingAction>,
}
```
