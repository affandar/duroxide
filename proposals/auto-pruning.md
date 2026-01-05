# Proposal: Automatic Execution Pruning

**Status:** Draft
**Author:** TBD
**Created:** 2024-01-04

## Summary

This proposal explores options for automatic execution pruning in long-running orchestrations that use `continue_as_new`. Currently, users must manually implement pruning via activities (see `sample_self_pruning_eternal_orchestration`). This proposal evaluates system-level alternatives.

## Problem Statement

Eternal orchestrations using `continue_as_new` accumulate execution history over time:

```
Execution 1 → ContinueAsNew → Execution 2 → ContinueAsNew → Execution 3 → ...
```

Each execution retains its full event history. For orchestrations running indefinitely (queue processors, schedulers, monitors), this leads to:

1. **Unbounded storage growth** - Each execution adds to total storage
2. **Slower queries** - `list_executions()` returns growing lists
3. **Manual cleanup burden** - Users must implement pruning logic

## Current Solution: Activity-Based Pruning

Users can implement self-pruning via an activity:

```rust
.register("PruneSelf", |ctx: ActivityContext, _: String| async move {
    let client = ctx.get_client();
    client.prune_executions(ctx.instance_id(), PruneOptions {
        keep_last: Some(1),
        ..Default::default()
    }).await.map_err(|e| e.to_string())?;
    Ok("pruned".to_string())
})

// In orchestration:
ctx.schedule_activity("PruneSelf", "").into_activity().await?;
ctx.continue_as_new(next_state).await
```

**Pros:**
- Explicit, visible in orchestration code
- Flexible timing and options
- No framework changes needed

**Cons:**
- Boilerplate in every eternal orchestration
- Easy to forget
- Activity overhead (scheduling, execution, history events)
- Must be called before `continue_as_new` (ordering matters)

---

## Proposed Alternatives

### Option A: System Activity (like `ctx.new_guid()`)

Add a built-in system operation that prunes without user-defined activities:

```rust
// Prune all but current execution
ctx.prune_history(PruneOptions { keep_last: Some(1), ..Default::default() }).await;
ctx.continue_as_new(next_state).await
```

**Implementation:**
- New `Action::PruneHistory { options }` variant
- Runtime handles pruning during orchestration dispatch
- Records `HistoryPruned { executions_deleted, events_deleted }` event

**Pros:**
- No activity registration required
- Deterministic (replays correctly)
- Lower overhead than activity (no queue, no worker dispatch)
- Explicit in orchestration code

**Cons:**
- New action type and event type
- Still requires user to remember to call it
- Adds complexity to replay engine

**User Personas:**
- *Power users*: Appreciate explicit control
- *New users*: May still forget to add it

---

### Option B: Parameter on `continue_as_new()`

Combine pruning with the continue operation:

```rust
// Option B1: Simple boolean
ctx.continue_as_new_with_prune(next_state, true).await

// Option B2: With options
ctx.continue_as_new_pruned(next_state, PruneOptions {
    keep_last: Some(3),  // Keep last 3 executions
    ..Default::default()
}).await

// Option B3: Builder pattern
ctx.continue_as_new(next_state)
    .prune_before(PruneOptions { keep_last: Some(1), ..Default::default() })
    .await
```

**Implementation:**
- Extend `Action::ContinueAsNew` with optional `prune_options: Option<PruneOptions>`
- Runtime prunes before starting new execution (atomic)

**Pros:**
- Natural pairing (prune and continue are logically linked)
- Single call instead of two
- Atomic operation (prune + continue together)
- Hard to get ordering wrong

**Cons:**
- API proliferation (`continue_as_new` vs `continue_as_new_pruned` vs `continue_as_new_with_options`)
- Less flexible if user wants to prune at other times
- Couples two concerns

**User Personas:**
- *New users*: Clear, hard to misuse
- *Power users*: May want pruning decoupled from continue

---

### Option C: Orchestration Registration Option

Declare pruning policy at orchestration definition:

```rust
OrchestrationRegistry::builder()
    .register_with_options(
        "EternalProcessor",
        orchestration_fn,
        OrchestrationOptions {
            auto_prune: Some(AutoPrunePolicy {
                keep_last: 1,
                on: PruneTrigger::ContinueAsNew,  // or ::EveryNExecutions(10)
            }),
            ..Default::default()
        }
    )
    .build()
```

**Implementation:**
- Store policy in orchestration metadata
- Runtime automatically prunes based on trigger condition
- No orchestration code changes needed

**Pros:**
- Zero orchestration code changes
- Policy defined once at registration
- Consistent across all instances of that orchestration
- "Set and forget"

**Cons:**
- Hidden behavior (not visible in orchestration code)
- Less flexible per-instance customization
- New registration API
- Policy might not fit all instances

**User Personas:**
- *Ops/Platform teams*: Love declarative policies
- *Developers*: May be surprised by implicit behavior
- *Debuggers*: "Where did my history go?"

---

### Option D: Runtime Configuration

Global or per-orchestration-type configuration:

```rust
// runtime_config.toml
[pruning]
enabled = true
default_keep_last = 5
triggers = ["continue_as_new"]

[pruning.overrides."EternalProcessor"]
keep_last = 1

// Or in code:
RuntimeOptions {
    auto_prune: AutoPruneConfig {
        default_policy: Some(PrunePolicy { keep_last: 5 }),
        overrides: hashmap! {
            "EternalProcessor" => PrunePolicy { keep_last: 1 },
        },
    },
    ..Default::default()
}
```

**Implementation:**
- Runtime checks config on each `continue_as_new`
- Applies policy based on orchestration name

**Pros:**
- No code changes to orchestrations
- Ops can tune without redeployment
- Centralized policy management
- Easy to enable/disable globally

**Cons:**
- Configuration complexity
- Disconnected from orchestration logic
- Hard to test (behavior depends on config)
- "Spooky action at a distance"

**User Personas:**
- *Ops teams*: Perfect for production tuning
- *Developers*: Frustrating when behavior differs between environments
- *Testers*: Need to replicate production config

---

## Comparison Matrix

| Criteria | Activity | System Op | continue_as_new param | Registration | Runtime Config |
|----------|----------|-----------|----------------------|--------------|----------------|
| Explicit in code | ✅ | ✅ | ✅ | ❌ | ❌ |
| Zero boilerplate | ❌ | ❌ | ⚠️ | ✅ | ✅ |
| Flexible timing | ✅ | ✅ | ❌ | ❌ | ❌ |
| Hard to forget | ❌ | ❌ | ⚠️ | ✅ | ✅ |
| Low overhead | ❌ | ✅ | ✅ | ✅ | ✅ |
| Testable | ✅ | ✅ | ✅ | ⚠️ | ❌ |
| No API changes | ✅ | ❌ | ❌ | ❌ | ❌ |
| Ops-friendly | ❌ | ❌ | ❌ | ⚠️ | ✅ |

---

## Recommendation

Implement in phases:

### Phase 1: System Activity (Option A)
Add `ctx.prune_history()` as a low-overhead system operation. This gives users explicit control with less boilerplate than the activity approach.

```rust
ctx.prune_history(PruneOptions { keep_last: Some(1), ..Default::default() }).await;
ctx.continue_as_new(state).await
```

### Phase 2: Registration Option (Option C)
For users who want "set and forget", add declarative policy at registration. This complements Phase 1 for different use cases.

```rust
.register_with_options("Eternal", fn, OrchestrationOptions {
    auto_prune_on_continue: Some(1),  // keep_last
    ..Default::default()
})
```

### Phase 3 (Maybe): Runtime Config (Option D)
If there's demand from ops teams, add runtime configuration as an override mechanism. This should be additive, not replacing the code-level options.

---

## Interaction with Versioning and Replay

### Why Pruning is Safe for Replay

Each execution is **self-contained** with its own complete history:

```
Execution 1: [OrchStarted] → [ActivityScheduled] → [ActivityCompleted] → [ContinuedAsNew]
Execution 2: [OrchStarted] → [TimerScheduled] → [TimerFired] → [ContinuedAsNew]
Execution 3: [OrchStarted] → [ActivityScheduled] → ... (current)
```

When replaying execution 3 after a crash:
- Runtime loads execution 3's history only
- Replays from its `OrchestrationStarted` event
- Executions 1 and 2 are **irrelevant** to replay

**Pruning executions 1 and 2 does not affect execution 3's replay correctness.**

### Version Transitions

Consider an orchestration upgrading across versions:

```
Exec 1 (v1.0): ProcessBatch → ContinueAsNew
Exec 2 (v1.0): ProcessBatch → ContinueAsNew
Exec 3 (v2.0): ProcessBatchV2 → ContinueAsNew  ← version upgrade here
Exec 4 (v2.0): ProcessBatchV2 → ...
```

**Scenario A: Prune during normal operation**
- Prune keeping last 2 executions (3 and 4)
- Execution 3 replays correctly using v2.0 code
- Execution 4 replays correctly using v2.0 code
- ✅ Safe

**Scenario B: Rollback to v1.0 after pruning**
- Executions 1 and 2 (v1.0) are gone
- Execution 3 and 4 have v2.0 history (different activity names/shapes)
- Replaying with v1.0 code → **NonDeterminismError**
- ❌ Cannot rollback

**Scenario C: Debugging v1.0 behavior**
- Bug reported: "v1.0 produced wrong output"
- Executions 1 and 2 are pruned
- Cannot inspect what v1.0 did
- ❌ Lost observability

### Recommendations for Version Transitions

1. **Increase retention during upgrades**
   ```rust
   // During version transition, keep more history
   ctx.prune_history(PruneOptions {
       keep_last: Some(10),  // instead of 1
       ..Default::default()
   }).await;
   ```

2. **Time-based retention for auditing**
   ```rust
   // Keep executions from last 7 days regardless of count
   ctx.prune_history(PruneOptions {
       keep_last: Some(1),
       completed_before: Some(now - 7.days()),  // AND semantics
       ..Default::default()
   }).await;
   ```

3. **Disable auto-prune during rollout**
   ```rust
   RuntimeOptions {
       auto_prune: AutoPruneConfig {
           enabled: !is_canary_deployment(),
           ..Default::default()
       },
   }
   ```

### What's Preserved vs Lost

| Data | After Pruning | Impact |
|------|---------------|--------|
| Current execution history | ✅ Preserved | Replay works |
| Current execution input | ✅ Preserved | State available |
| Previous execution outputs | ❌ Lost | Can't inspect old results |
| Previous version behavior | ❌ Lost | Can't debug old code |
| Sub-orchestration references | ⚠️ Partial | Sub-orch exists, but parent's spawn record gone |
| Event delivery records | ❌ Lost | Can't see events raised to old executions |

### Sub-Orchestrations and Pruning

Pruning parent executions does **not** cascade to sub-orchestrations:

```
Parent Exec 1: SpawnChild(C1) → ContinueAsNew
Parent Exec 2: SpawnChild(C2) → ContinueAsNew
Parent Exec 3: ... (current)

Child C1: [independent instance with own history]
Child C2: [independent instance with own history]
```

If we prune parent execution 1:
- Child C1 **still exists** (separate instance)
- Parent's `SubOrchestrationScheduled` event for C1 is **gone**
- We lose the audit trail of "who spawned C1"

### Unobserved Sub-Orchestration Completions

A more subtle issue arises with sub-orchestrations that are spawned but not awaited:

**Scenario 1: Select2 loser**
```
Parent Exec 1:
  - SubOrchScheduled(C1, event_id=2)
  - SubOrchScheduled(C2, event_id=3)
  - Select2 → C1 wins
  - SubOrchCompleted(C1)
  - ContinueAsNew            ← C2 still running!

Parent Exec 2:
  - Prune(keep_last=1)       ← Exec 1 deleted
  - ...working...

Child C2: completes, sends SubOrchCompleted to parent
```

**Scenario 2: Unawaited DurableFuture**
```rust
// Parent code - BUG: forgot to await child2
let child1 = ctx.schedule_sub_orchestration("Fast", "").into_sub_orchestration().await?;
let child2 = ctx.schedule_sub_orchestration("Slow", "");  // never awaited!
ctx.continue_as_new(state).await
```

```
Parent Exec 1:
  - SubOrchScheduled(C1)
  - SubOrchCompleted(C1)
  - SubOrchScheduled(C2)     ← scheduled but never awaited
  - ContinueAsNew

Child C2: running, will complete eventually
```

**What happens when orphaned child completes?**

1. Child C2 completes, runtime appends `SubOrchCompleted` to parent's history
2. Parent's current execution (Exec 2) replays
3. Exec 2's code never scheduled C2, so completion is **ignored** (execution_id filtering)
4. Same behavior with or without pruning

**Key insight:** Pruning doesn't create new problems here - the orphaned completion was already going to be ignored due to execution_id mismatch. The existing `continue_as_new` semantics handle this.

### The Real Problem: Resource Leaks

The issue isn't correctness, it's **cleanup**:

```
After pruning Exec 1:
- C2 instance still exists in database
- C2 has parent_instance_id = "parent"
- C2 might be: Running, Completed, or Failed
- Nobody is waiting for C2
- C2 is effectively orphaned
```

**Current state:**
| Child State | Parent Exec Pruned? | Outcome |
|-------------|---------------------|---------|
| Running | No | Select2 loser continues running |
| Running | Yes | Same - still running, orphaned |
| Completed | No | Completion ignored by parent |
| Completed | Yes | Same - completed, orphaned |
| Failed | No | Failure ignored by parent |
| Failed | Yes | Same - failed, orphaned |

Pruning doesn't change the orphaned child's fate - it was already orphaned by the select2 or unawaited future.

### Recommendations

1. **For select2 with sub-orchestrations: Cancel the loser**
   ```rust
   let (winner_idx, result) = ctx.select2(child1, child2).await;
   // Explicitly cancel the loser
   if winner_idx == 0 {
       // child2 lost - it gets cancelled automatically by select2
   }
   ```

   Note: `select2` already cancels the loser. But if cancellation doesn't propagate (child ignores it), the child continues.

2. **For fire-and-forget: Use detached orchestrations**
   ```rust
   // Use schedule_orchestration (detached) instead of schedule_sub_orchestration
   ctx.schedule_orchestration("Worker", "child-1", input);
   // Detached orchestrations are independent - no parent relationship
   ```

3. **Consider adding orphan cleanup (future feature)**
   ```rust
   // Management API to find and clean orphaned sub-orchestrations
   client.cleanup_orphaned_instances(OrphanFilter {
       parent_completed_before: Some(now - 7.days()),
       child_status: vec![Status::Completed, Status::Failed],
   }).await;
   ```

4. **Audit logging for spawn events**
   If you need to track "who spawned whom" after pruning, emit explicit trace events:
   ```rust
   ctx.trace_info(&format!("Spawning sub-orchestration: {}", child_id));
   let child = ctx.schedule_sub_orchestration("Worker", input);
   ```

### Summary: Pruning + Unobserved Completions

| Concern | Impact | Mitigation |
|---------|--------|------------|
| Correctness | ✅ None - execution_id filtering already handles this | N/A |
| Orphaned running children | ⚠️ Continue running | Cancel losers explicitly |
| Orphaned completed children | ⚠️ Sit in DB forever | Future: orphan cleanup API |
| Lost spawn audit trail | ⚠️ Can't trace lineage | Use explicit trace events |

**Recommendation:** For orchestrations with sub-orchestrations, consider keeping more history or using separate audit logging.

### Nondeterminism Detection

Nondeterminism is detected **within a single execution** during replay:

```
Execution 3 Replay:
  History: [OrchStarted, ActivityScheduled(id=2, name="Foo")]
  Code:    ctx.schedule_activity("Bar", ...)  // Different name!
  Result:  NonDeterminismError
```

Pruning old executions has **no effect** on nondeterminism detection because:
1. Detection compares code behavior vs current execution's history
2. Old executions' histories are never consulted during replay
3. Each execution is replayed independently

### Edge Case: Crash During Prune + ContinueAsNew

If using atomic prune-and-continue (Option B):

```
1. Orchestration calls continue_as_new_pruned(state, keep_last=1)
2. Runtime starts transaction:
   a. Delete old executions
   b. Create new execution
   c. CRASH before commit
3. On recovery: transaction rolled back
4. Old executions still exist, new execution not created
5. Orchestration replays, calls continue_as_new_pruned again
6. ✅ Idempotent, no data loss
```

If using separate prune-then-continue:

```
1. Orchestration calls prune_history(keep_last=1)
2. Prune completes, event recorded
3. Orchestration calls continue_as_new(state)
4. CRASH before continue completes
5. On recovery: prune event in history
6. Replay: prune is idempotent (already done), continue proceeds
7. ✅ Safe, but two operations instead of one
```

---

## Open Questions

1. **Should pruning be sync or async?** System activity could be fire-and-forget (don't wait for completion) to minimize latency.

2. **What if pruning fails?** Should it block `continue_as_new`? Probably not - pruning failure shouldn't break the orchestration.

3. **Metrics/observability?** Should emit metrics for pruned executions (e.g., `duroxide_executions_pruned_total`).

4. **Interaction with `completed_before`?** Should auto-prune support time-based retention, or just count-based?

5. **Per-instance override?** Can an instance opt-out of registration-level auto-prune?

---

## Appendix: Event Schema

If implementing Option A (System Activity):

```rust
enum EventKind {
    // ... existing variants ...

    /// History pruning was requested
    HistoryPruneRequested {
        options: PruneOptions,
    },

    /// History pruning completed
    HistoryPruned {
        executions_deleted: u64,
        events_deleted: u64,
    },
}

enum Action {
    // ... existing variants ...

    PruneHistory {
        scheduling_event_id: u64,
        options: PruneOptions,
    },
}
```
