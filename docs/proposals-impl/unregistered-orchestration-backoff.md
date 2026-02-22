# Unregistered Orchestration/Activity Backoff

**Status:** Implemented  
**Issue:** [#47](https://github.com/microsoft/duroxide/issues/47)

## Overview

Handle unregistered orchestrations and activities during rolling deployments by abandoning work items with exponential backoff instead of immediately terminating them. Leverage the existing poison message infrastructure for eventual termination.

## Problem Statement

During rolling deployments:

1. New code registers a new orchestration or activity (e.g., `ActivityV2`)
2. Old workers (still running the previous version) may pick up work items for the new orchestration/activity
3. Currently, these items are **immediately terminated/failed** with `ConfigurationError::UnregisteredOrchestration` or `ConfigurationError::UnregisteredActivity`
4. This causes **data loss and workflow failures** even though the code exists on other pods

### Current Behavior

**Orchestrations** (orchestration.rs#L620-L650):
```rust
None => {
    // Not found in registry - fail with unregistered error
    if history_mgr.is_empty() {
        history_mgr.append_failed(crate::ErrorDetails::Configuration {
            kind: crate::ConfigErrorKind::UnregisteredOrchestration,
            resource: workitem_reader.orchestration_name.clone(),
            message: None,
        });
    }
    return (worker_items, orchestrator_items, cancelled_activities);
}
```

**Activities** (worker.rs#L530-L545):
```rust
let ack_result = rt.history_store.ack_work_item(
    &ctx.lock_token,
    Some(WorkItem::ActivityFailed {
        instance: ctx.instance.clone(),
        execution_id: ctx.execution_id,
        id: ctx.activity_id,
        details: crate::ErrorDetails::Configuration {
            kind: crate::ConfigErrorKind::UnregisteredActivity,
            resource: ctx.activity_name.clone(),
            message: None,
        },
    }),
).await;
```

## Goals

1. **Rolling deployments just work** - No special coordination needed
2. **Graceful degradation** - Eventually fails if code is genuinely missing (via poison message handling)
3. **Observable** - Metrics and logs show what's happening (escalates to ERROR on final attempt)
4. **Configurable** - Backoff timing can be tuned for different deployment scenarios
5. **Backward compatible** - Behavior after max attempts matches current behavior

## Non-Goals

- New configuration options for unregistered-specific max attempts (use existing `max_attempts`)
- Distinguishing "poison due to unregistered" from other poison errors (same terminal state)

---

## Design

### Core Insight

**Unregistered = "not ready yet" during rolling deployment.**

Instead of immediately failing, abandon the work item back to the queue with a visibility delay. The existing poison message infrastructure handles the case where the code is genuinely missing:

```
Fetch #1 → not registered → abandon with 1s delay
Fetch #2 → not registered → abandon with 2s delay  
Fetch #3 → picked up by new pod → executes successfully ✓

OR (genuinely missing):

Fetch #1  → abandon with 1s delay
Fetch #2  → abandon with 2s delay
...
Fetch #10 → attempt_count exceeds max_attempts → POISON → fail
```

### Why Reuse Poison Message Handling?

The comment from the issue captures it well:

> We should ideally just use the poison message handling infrastructure to finally remove the message if it was genuine missing orchestration. No need to invent something special (i.e. no need for unregistered_max_attempts).

Benefits:
- No new configuration options
- Consistent behavior with other "repeatedly failing" scenarios
- Single code path for "message keeps bouncing"
- Already tested and production-ready

### Exponential Backoff Calculation

```rust
/// Configuration for unregistered orchestration/activity backoff
#[derive(Debug, Clone)]
pub struct UnregisteredBackoffConfig {
    /// Base delay for first backoff (default: 1 second)
    pub base_delay: Duration,
    /// Maximum delay cap (default: 60 seconds)
    pub max_delay: Duration,
}

impl Default for UnregisteredBackoffConfig {
    fn default() -> Self {
        Self {
            base_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(60),
        }
    }
}

fn calculate_unregistered_backoff(attempt_count: u32, config: &UnregisteredBackoffConfig) -> Duration {
    // attempt_count is 1-based from the provider
    let exponent = attempt_count.saturating_sub(1).min(6); // Cap exponent at 6 (64x base)
    let delay = config.base_delay.saturating_mul(1 << exponent);
    delay.min(config.max_delay)
}

// Example delays with default config (1s base, 60s max):
// Attempt 1: 1s
// Attempt 2: 2s
// Attempt 3: 4s
// Attempt 4: 8s
// Attempt 5: 16s
// Attempt 6: 32s
// Attempt 7+: 60s (capped)
```

### Logging

Use WARN level for all backoff attempts with a consistent message format showing remaining attempts before poison. This is user error (missing handler registration), not infrastructure error:

```rust
fn log_unregistered_orchestration_backoff(
    orchestration_name: &str,
    version: &str,
    instance: &str,
    attempt_count: u32,
    max_attempts: u32,
    backoff: Duration,
) {
    // Defensive: use >= even though poison check uses >
    let remaining = if attempt_count >= max_attempts {
        0
    } else {
        max_attempts - attempt_count
    };
    
    tracing::warn!(
        target: "duroxide::runtime",
        instance = %instance,
        orchestration_name = %orchestration_name,
        version = %version,
        attempt_count = %attempt_count,
        max_attempts = %max_attempts,
        remaining_attempts = %remaining,
        backoff_secs = %backoff.as_secs_f32(),
        "Orchestration not registered, abandoning with {:.1}s backoff (will poison in {} more attempts)",
        backoff.as_secs_f32(),
        remaining
    );
}

fn log_unregistered_activity_backoff(
    activity_name: &str,
    instance: &str,
    attempt_count: u32,
    max_attempts: u32,
    backoff: Duration,
) {
    let remaining = if attempt_count >= max_attempts {
        0
    } else {
        max_attempts - attempt_count
    };
    
    tracing::warn!(
        target: "duroxide::runtime",
        instance = %instance,
        activity_name = %activity_name,
        attempt_count = %attempt_count,
        max_attempts = %max_attempts,
        remaining_attempts = %remaining,
        backoff_secs = %backoff.as_secs_f32(),
        "Activity not registered, abandoning with {:.1}s backoff (will poison in {} more attempts)",
        backoff.as_secs_f32(),
        remaining
    );
}
```

---

## Implementation

### Architecture Insight

**HistoryManager is purely in-memory** - it builds up events/actions but does no I/O. The dispatcher handles all I/O (ack, abandon). 

Instead of calling `abandon_orchestration_item` directly when an orchestration is unregistered, we:
1. Set a state on HistoryManager indicating "abandon with backoff"
2. The dispatcher checks this state after processing returns
3. The dispatcher handles the I/O (abandon with delay)

This keeps the separation of concerns clean and reuses the existing dispatcher flow.

### Changes Required

#### 1. Remove ConfigErrorKind variants

Remove `ConfigErrorKind::UnregisteredOrchestration` and `ConfigErrorKind::UnregisteredActivity`. These error types are no longer needed since unregistered handlers now trigger backoff → poison flow instead of immediate failure.

```rust
// BEFORE: in src/lib.rs
pub enum ConfigErrorKind {
    UnregisteredOrchestration,
    UnregisteredActivity,
    // ... other variants
}

// AFTER: Remove UnregisteredOrchestration and UnregisteredActivity
// (If there are no other variants, the entire ConfigErrorKind may be removed)
```

#### 2. Orchestration Dispatcher (`orchestration.rs`)

No changes to HistoryManager are needed. The handler resolution and backoff handling can all happen in the dispatcher. When the handler is not found, we simply abandon with backoff instead of appending failure events.

**In `handle_orchestration_atomic` - handler resolution:**

```rust
// BEFORE:
None => {
    // Not found in registry - fail with unregistered error
    if history_mgr.is_empty() {
        history_mgr.append(Event::with_event_id(...OrchestrationStarted...));
        history_mgr.append_failed(crate::ErrorDetails::Configuration {
            kind: crate::ConfigErrorKind::UnregisteredOrchestration,
            ...
        });
    }
    return (worker_items, orchestrator_items, cancelled_activities);
}

// AFTER:
None => {
    // Not found in registry - abandon with backoff for rolling deployment support
    // Don't create any history events - let poison handling create them if genuinely missing
    
    let backoff = calculate_unregistered_backoff(attempt_count, &self.options.unregistered_backoff);
    let remaining = if attempt_count >= self.options.max_attempts {
        0
    } else {
        self.options.max_attempts - attempt_count
    };
    
    tracing::warn!(
        target: "duroxide::runtime",
        instance = %instance,
        orchestration_name = %workitem_reader.orchestration_name,
        version = %workitem_reader.version.as_deref().unwrap_or("latest"),
        attempt_count = %attempt_count,
        max_attempts = %self.options.max_attempts,
        remaining_attempts = %remaining,
        backoff_secs = %backoff.as_secs_f32(),
        "Orchestration not registered, abandoning with {:.1}s backoff (will poison in {} more attempts)",
        backoff.as_secs_f32(),
        remaining
    );
    
    // Abandon with delay - let poison handling eventually terminate if genuinely missing
    let _ = self.history_store
        .abandon_orchestration_item(lock_token, Some(backoff), false)
        .await;
    
    return (worker_items, orchestrator_items, cancelled_activities);
}
```

#### 3. Worker Dispatcher (`worker.rs`)

Similar pattern - instead of acking with `ActivityFailed`, abandon with backoff:

```rust
// In execute_activity or handle_unregistered_activity

// BEFORE:
if !activities.contains(&ctx.activity_name) {
    let ack_result = rt.history_store.ack_work_item(
        &ctx.lock_token,
        Some(WorkItem::ActivityFailed {
            details: crate::ErrorDetails::Configuration {
                kind: crate::ConfigErrorKind::UnregisteredActivity,
                ...
            },
        }),
    ).await;
}

// AFTER:
if !activities.contains(&ctx.activity_name) {
    let backoff = calculate_unregistered_backoff(ctx.attempt_count, &rt.options().unregistered_backoff);
    
    log_unregistered_activity_backoff(
        &ctx.activity_name,
        &ctx.instance,
        ctx.attempt_count,
        rt.options().max_attempts,
        backoff,
    );
    
    // Abandon with delay - let poison handling eventually terminate if genuinely missing
    let _ = rt.history_store
        .abandon_work_item(&ctx.lock_token, Some(backoff), false)
        .await;
    
    return;
}
```

#### 4. RuntimeOptions

Add configurable backoff settings:

```rust
pub struct RuntimeOptions {
    // ... existing fields ...
    
    /// Configuration for backoff when encountering unregistered orchestrations/activities.
    /// This allows work items to be retried during rolling deployments instead of
    /// immediately failing.
    pub unregistered_backoff: UnregisteredBackoffConfig,
}

impl Default for RuntimeOptions {
    fn default() -> Self {
        Self {
            // ... existing defaults ...
            unregistered_backoff: UnregisteredBackoffConfig::default(),
        }
    }
}
```

---

## Example Timeline (Rolling Deployment)

```
T+0:00   New code deployed to pod-1 (registers ActivityV2)
T+0:05   Work item for ActivityV2 picked up by pod-2 (old code)
         → pod-2: "Activity not registered, abandoning with backoff"
         → abandon with 1s delay, attempt_count=1
         
T+0:06   Work item visible again, picked up by pod-2
         → abandon with 2s delay, attempt_count=2
         
T+0:08   pod-2 gets new code (rolling update)

T+0:10   Work item visible again, picked up by pod-2 (new code)
         → ActivityV2 executes successfully ✓
```

## Example Timeline (Genuinely Missing Code)

```
T+0:00   Work item for "BogusOrch" enqueued (typo in orchestration name)

T+0:01   Fetch #1 → not registered → abandon with 1s delay
T+0:02   Fetch #2 → not registered → abandon with 2s delay
T+0:04   Fetch #3 → not registered → abandon with 4s delay
T+0:08   Fetch #4 → not registered → abandon with 8s delay
T+0:16   Fetch #5 → not registered → abandon with 16s delay
T+0:32   Fetch #6 → not registered → abandon with 32s delay
T+1:32   Fetch #7 → not registered → abandon with 60s delay
T+2:32   Fetch #8 → not registered → abandon with 60s delay
T+3:32   Fetch #9 → not registered → abandon with 60s delay
T+4:32   Fetch #10 → not registered → abandon with 60s delay

T+5:32   Fetch #11 → attempt_count=11 > max_attempts=10
         → POISON MESSAGE HANDLING kicks in
         → Orchestration created with Failed status
         → ErrorDetails::Poison { ... }
```

Total time before failure: ~5.5 minutes (acceptable for detecting genuinely missing code)

---

## Edge Cases

### 1. Versioned Orchestrations

When resolving by exact version (e.g., `version: "2.0.0"`), the same logic applies:
- If version not found → abandon with backoff
- Eventually poison if version never appears

### 2. Sub-orchestrations

Sub-orchestrations follow the same pattern. If a child orchestration is not registered:
- Child abandons with backoff
- Parent waits (normal sub-orchestration timeout behavior)
- Eventually child poison → parent fails with child's poison error

### 3. Mixed Fleet (Some Pods Never Get New Code)

If some pods are permanently stuck on old code:
- Those pods repeatedly abandon the work item
- Eventually, poison threshold is reached
- Work item fails with poison error
- This is the correct behavior - the work item is genuinely unprocessable by the current fleet

### 4. Very Short max_attempts

With `max_attempts: 3`, unregistered items fail quickly:
- Fetch #1 → abandon 1s
- Fetch #2 → abandon 2s  
- Fetch #3 → abandon 4s
- Fetch #4 → POISON

Total: ~7 seconds. Consider increasing `max_attempts` for clusters with slower rollouts.

---

## Testing

### Unit Tests

1. `test_backoff_calculation_exponential`
2. `test_backoff_caps_at_max`
3. `test_backoff_respects_custom_config`

### Integration Tests (`tests/deletion_integration_tests.rs`)

1. `test_cancel_orchestration_during_unregistered_backoff` - Cancel an orchestration that is bouncing with backoff (not yet poisoned)
2. `test_force_delete_orchestration_during_unregistered_backoff` - Force delete an orchestration during backoff
3. `test_delete_poisoned_orchestration` - Verify that after poison, orchestration can be deleted gracefully (status is Failed, no force needed)

### Unregistered Handler Tests (`tests/unknown_orchestration_tests.rs`)

**Existing tests to update (currently expect ConfigError, will need to expect Poison after backoff):**
1. `unknown_orchestration_fails_gracefully` - Missing orchestration name

**New tests to add:**
1. `unknown_version_fails_gracefully` - Existing orchestration name but missing version (e.g., `start_orchestration_versioned("inst", "ExistingOrch", "9.9.9", "")`)
2. `continue_as_new_to_missing_version_fails_gracefully` - CAN switches to a version that doesn't exist

### Scenario Tests (`tests/scenarios/rolling_deployment.rs`)

1. `e2e_rolling_deployment_three_nodes` - Simulates real rolling upgrade with missing orchestration/activity
2. `e2e_rolling_deployment_version_upgrade` - CAN switches to new version during rolling deployment (old nodes don't have new version yet)

### Scenario Test: Rolling Deployment Simulation (`tests/scenarios/rolling_deployment.rs`)

This file contains two tests simulating realistic rolling upgrades:

#### Test 1: `e2e_rolling_deployment_three_nodes`

Simulates rolling deployment with a new activity:

```rust
/// E2E: Simulates rolling deployment across 3 nodes
///
/// Scenario:
/// - 3 "nodes" (runtimes) sharing same provider
/// - Initially: 2 nodes have old code (no NewActivity), 1 node has new code
/// - Work item for NewActivity is enqueued
/// - Old nodes repeatedly abandon with backoff
/// - After 2 seconds: old nodes "upgrade" (get new code)
/// - Work item eventually succeeds on upgraded node
```

#### Test 2: `e2e_rolling_deployment_version_upgrade`

Simulates CAN (continue-as-new) switching to a new version during rolling deployment:

```rust
/// E2E: Simulates rolling deployment with version upgrade via CAN
///
/// Scenario:
/// - Orchestration v1.0.0 is running, does continue_as_new to v2.0.0
/// - 2 nodes have only v1.0.0, 1 node has both v1.0.0 and v2.0.0
/// - CAN message for v2.0.0 bounces on old nodes with backoff
/// - After upgrade, v2.0.0 executes successfully
///
/// This validates that version-specific backoff works correctly during
/// rolling deployments where new versions are introduced.
#[tokio::test]
async fn e2e_rolling_deployment_version_upgrade() {
    let provider = Arc::new(
        SqliteProvider::new_in_memory()
            .await
            .expect("Failed to create provider"),
    );
    
    // v1.0.0 handler - does CAN to v2.0.0
    fn v1_handler() -> impl Fn(OrchestrationContext, String) -> _ {
        |ctx: OrchestrationContext, _input: String| async move {
            // Continue as new to v2.0.0
            ctx.continue_as_new_versioned("2.0.0", "upgraded").await
        }
    }
    
    // v2.0.0 handler - just completes
    fn v2_handler() -> impl Fn(OrchestrationContext, String) -> _ {
        |_ctx: OrchestrationContext, input: String| async move {
            Ok::<_, String>(format!("v2-completed:{}", input))
        }
    }
    
    // Old nodes: only have v1.0.0
    let orchestrations_old = OrchestrationRegistry::builder()
        .register_versioned("VersionedOrch", "1.0.0", v1_handler())
        .build();
    
    // New node: has both v1.0.0 and v2.0.0
    let orchestrations_new = OrchestrationRegistry::builder()
        .register_versioned("VersionedOrch", "1.0.0", v1_handler())
        .register_versioned("VersionedOrch", "2.0.0", v2_handler())
        .build();
    
    let activities = Arc::new(ActivityRegistry::builder().build());
    
    let options = RuntimeOptions {
        max_attempts: 10,
        dispatcher_min_poll_interval: Duration::from_millis(10),
        unregistered_backoff: UnregisteredBackoffConfig {
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_millis(500),
        },
        ..Default::default()
    };
    
    // Start nodes: 2 old (v1 only), 1 new (v1+v2)
    let rt1 = runtime::Runtime::start_with_options(
        provider.clone(), activities.clone(), orchestrations_old.clone(), options.clone(),
    ).await;
    let rt2 = runtime::Runtime::start_with_options(
        provider.clone(), activities.clone(), orchestrations_old.clone(), options.clone(),
    ).await;
    let rt3 = runtime::Runtime::start_with_options(
        provider.clone(), activities.clone(), orchestrations_new.clone(), options.clone(),
    ).await;
    
    let client = Client::new(provider.clone());
    
    // Start orchestration at v1.0.0 - it will CAN to v2.0.0
    client
        .start_orchestration_versioned("version-upgrade-test", "VersionedOrch", "1.0.0", "{}")
        .await
        .expect("start should succeed");
    
    // Simulate rolling upgrade: after 2 seconds, upgrade old nodes
    tokio::spawn({
        let rt1 = rt1.clone();
        let rt2 = rt2.clone();
        let provider = provider.clone();
        let activities = activities.clone();
        let orchestrations_new = orchestrations_new.clone();
        let options = options.clone();
        async move {
            tokio::time::sleep(Duration::from_secs(2)).await;
            rt1.shutdown(None).await;
            let _rt1_new = runtime::Runtime::start_with_options(
                provider.clone(), activities.clone(), orchestrations_new.clone(), options.clone(),
            ).await;
            
            tokio::time::sleep(Duration::from_secs(1)).await;
            rt2.shutdown(None).await;
            let _rt2_new = runtime::Runtime::start_with_options(
                provider.clone(), activities.clone(), orchestrations_new.clone(), options.clone(),
            ).await;
        }
    });
    
    // Wait for orchestration to complete
    let status = client
        .wait_for_orchestration("version-upgrade-test", Duration::from_secs(10))
        .await
        .expect("wait should succeed");
    
    // Should complete successfully with v2 output
    match status {
        OrchestrationStatus::Completed { output } => {
            assert!(
                output.contains("v2-completed"),
                "Should have v2 output, got: {}", output
            );
        }
        OrchestrationStatus::Failed { details } => {
            panic!(
                "Orchestration should NOT fail during rolling deployment. \
                 CAN to v2.0.0 should have bounced until upgraded node picked it up. \
                 Error: {details:?}"
            );
        }
        other => panic!("Unexpected status: {other:?}"),
    }
    
    rt3.shutdown(None).await;
}
```

```rust
/// E2E: Simulates rolling deployment across 3 nodes
///
/// Scenario:
/// - 3 "nodes" (runtimes) sharing same provider
/// - Initially: 2 nodes have old code (no NewActivity), 1 node has new code
/// - Work item for NewActivity is enqueued
/// - Old nodes repeatedly abandon with backoff
/// - After 2 seconds: old nodes "upgrade" (get new code)
/// - Work item eventually succeeds on upgraded node
///
/// This validates that unregistered backoff allows rolling deployments to succeed
/// without manual coordination.
#[tokio::test]
async fn e2e_rolling_deployment_three_nodes() {
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::sync::Arc;
    
    let provider = Arc::new(
        SqliteProvider::new_in_memory()
            .await
            .expect("Failed to create provider"),
    );
    
    // Track which "nodes" have been upgraded
    let node1_upgraded = Arc::new(AtomicBool::new(false));
    let node2_upgraded = Arc::new(AtomicBool::new(false));
    let activity_executed = Arc::new(AtomicBool::new(false));
    let backoff_count = Arc::new(AtomicU32::new(0));
    
    // All nodes share the same orchestration (calls NewActivity)
    let orchestrations = OrchestrationRegistry::builder()
        .register(
            "RollingDeployOrch",
            |ctx: OrchestrationContext, _input: String| async move {
                ctx.schedule_activity("NewActivity", "{}").into_activity().await
            },
        )
        .build();
    
    // Node 3 starts with new code (has NewActivity)
    let activity_executed_clone = activity_executed.clone();
    let activities_new = Arc::new(
        ActivityRegistry::builder()
            .register("NewActivity", move |_ctx: ActivityContext, _input: String| {
                let executed = activity_executed_clone.clone();
                async move {
                    executed.store(true, Ordering::SeqCst);
                    Ok::<_, String>("new-activity-result".to_string())
                }
            })
            .build()
    );
    
    // Nodes 1 & 2 start with old code (NO NewActivity registered)
    let activities_old = Arc::new(ActivityRegistry::builder().build());
    
    // Fast options with short backoff for testing
    let options = RuntimeOptions {
        max_attempts: 10,
        dispatcher_min_poll_interval: Duration::from_millis(10),
        unregistered_backoff: UnregisteredBackoffConfig {
            base_delay: Duration::from_millis(100), // 100ms base for fast test
            max_delay: Duration::from_millis(500),  // 500ms cap
        },
        ..Default::default()
    };
    
    // Start all 3 nodes
    // Node 1: old code
    let rt1 = runtime::Runtime::start_with_options(
        provider.clone(),
        activities_old.clone(),
        orchestrations.clone(),
        options.clone(),
    ).await;
    
    // Node 2: old code  
    let rt2 = runtime::Runtime::start_with_options(
        provider.clone(),
        activities_old.clone(),
        orchestrations.clone(),
        options.clone(),
    ).await;
    
    // Node 3: new code (has NewActivity)
    let rt3 = runtime::Runtime::start_with_options(
        provider.clone(),
        activities_new.clone(),
        orchestrations.clone(),
        options.clone(),
    ).await;
    
    let client = Client::new(provider.clone());
    
    // Start orchestration - will schedule NewActivity
    let instance = "rolling-deployment-test";
    client
        .start_orchestration(instance, "RollingDeployOrch", "{}")
        .await
        .expect("start should succeed");
    
    // Simulate rolling upgrade: after 2 seconds, upgrade nodes 1 & 2
    let node1_upgraded_clone = node1_upgraded.clone();
    let node2_upgraded_clone = node2_upgraded.clone();
    let rt1_handle = rt1.clone();
    let rt2_handle = rt2.clone();
    let provider_clone = provider.clone();
    let activities_new_clone = activities_new.clone();
    let orchestrations_clone = orchestrations.clone();
    let options_clone = options.clone();
    
    tokio::spawn(async move {
        // Wait 2 seconds (simulating rolling deployment window)
        tokio::time::sleep(Duration::from_secs(2)).await;
        
        // "Upgrade" node 1: shutdown old, start new
        rt1_handle.shutdown(None).await;
        node1_upgraded_clone.store(true, Ordering::SeqCst);
        let _rt1_new = runtime::Runtime::start_with_options(
            provider_clone.clone(),
            activities_new_clone.clone(),
            orchestrations_clone.clone(),
            options_clone.clone(),
        ).await;
        
        // Wait 1 more second, then upgrade node 2
        tokio::time::sleep(Duration::from_secs(1)).await;
        rt2_handle.shutdown(None).await;
        node2_upgraded_clone.store(true, Ordering::SeqCst);
        let _rt2_new = runtime::Runtime::start_with_options(
            provider_clone.clone(),
            activities_new_clone.clone(),
            orchestrations_clone.clone(),
            options_clone.clone(),
        ).await;
    });
    
    // Wait for orchestration to complete (should succeed after nodes upgrade)
    let status = client
        .wait_for_orchestration(instance, Duration::from_secs(10))
        .await
        .expect("wait should succeed");
    
    // Orchestration should complete successfully
    match status {
        OrchestrationStatus::Completed { output } => {
            assert!(
                output.contains("new-activity-result"),
                "Should have result from NewActivity"
            );
            assert!(
                activity_executed.load(Ordering::SeqCst),
                "NewActivity should have executed"
            );
        }
        OrchestrationStatus::Failed { details } => {
            panic!(
                "Orchestration should NOT fail during rolling deployment. \
                 Backoff should have kept retrying until upgraded node picked it up. \
                 Error: {details:?}"
            );
        }
        other => panic!("Unexpected status: {other:?}"),
    }
    
    // Cleanup
    rt3.shutdown(None).await;
}
```

### Fault Injection Tests

Use `PoisonInjectingProvider` pattern to simulate:
- Work item repeatedly picked up by "old" workers
- Eventually picked up by "new" worker with handler registered

---

## Migration

### Behavioral Change

This is a **breaking behavioral change** for users who rely on immediate failure of unregistered orchestrations/activities.

**Before:** Unregistered → immediate failure  
**After:** Unregistered → backoff → eventual poison failure

### Mitigation

1. **Logging:** WARN-level logs clearly indicate backoff behavior
2. **Metrics:** New counters show backoff events
3. **Configuration:** Users can reduce `max_attempts` for faster failure if needed
4. **Documentation:** Update ORCHESTRATION-GUIDE.md with new behavior

---

## Alternatives Considered

### 1. Separate Unregistered Max Attempts

```rust
pub unregistered_max_attempts: u32,
```

**Rejected:** Adds complexity without significant benefit. Reusing `max_attempts` is simpler and provides unified behavior for "repeatedly failing" messages.

### 2. Sticky Queues (Temporal-style)

Route work items to specific workers that have the handler registered.

**Rejected:** Requires significant queue infrastructure changes. Out of scope.

### 3. Immediate Failure with Retryable Error

Mark the error as retryable so clients can retry.

**Rejected:** Doesn't solve the rolling deployment problem - retries would still hit old pods.

### 4. Distinct Error Type for Poison-Due-To-Unregistered

Add `PoisonMessageType::UnregisteredOrchestration` variant.

**Rejected:** Adds complexity for marginal benefit. The terminal state is the same (Failed with Poison error), and logs provide enough context to understand the cause.

---

## Implementation Checklist

- [ ] Remove `ConfigErrorKind::UnregisteredOrchestration` and `ConfigErrorKind::UnregisteredActivity`
- [ ] Add `UnregisteredBackoffConfig` struct with `base_delay` and `max_delay`
- [ ] Add `unregistered_backoff` field to `RuntimeOptions`
- [ ] Add `calculate_unregistered_backoff()` helper function
- [ ] Modify `handle_orchestration_atomic` to abandon with backoff instead of failing
- [ ] Modify worker dispatcher to abandon unregistered activities with backoff
- [ ] Update tests in `unknown_orchestration_tests.rs`:
  - [ ] Update `unknown_orchestration_fails_gracefully` (expect Poison after backoff)
  - [ ] Add `unknown_version_fails_gracefully` (missing version of existing orchestration)
  - [ ] Add `continue_as_new_to_missing_version_fails_gracefully`
- [ ] Add unit tests for backoff calculation
- [ ] Add integration test: `test_cancel_orchestration_during_unregistered_backoff`
- [ ] Add integration test: `test_force_delete_orchestration_during_unregistered_backoff`
- [ ] Add integration test: `test_delete_poisoned_orchestration` (verify graceful delete after poison)
- [ ] Add scenario test: `e2e_rolling_deployment_three_nodes`
- [ ] Add scenario test: `e2e_rolling_deployment_version_upgrade` (CAN to new version)
- [ ] Update ORCHESTRATION-GUIDE.md
- [ ] Update CHANGELOG.md
