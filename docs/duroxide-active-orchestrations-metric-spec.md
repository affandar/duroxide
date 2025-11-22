# Duroxide Active Orchestrations Metric Specification

## Problem Statement

**Current Issue:**
Administrators cannot accurately track how many orchestrations are currently running. The calculation `duroxide_orchestration_starts_total - duroxide_orchestration_completions_total` is incorrect because both are cumulative counters that never decrease.

**Example of the Problem:**
```
Time T0: Start orchestration A → starts_total = 1, completions_total = 0
Time T1: Complete orchestration A → starts_total = 1, completions_total = 1
Time T2: Start orchestration B → starts_total = 2, completions_total = 1

Calculation: 2 - 1 = 1 active
Reality: 1 active ✅

But then:
Time T3: Complete orchestration B → starts_total = 2, completions_total = 2
Time T4: Restart app (counters reset) → starts_total = 0, completions_total = 0

Calculation after restart: 0 - 0 = 0
Reality: May have orchestrations from before restart still running! ❌
```

Additionally, with continue-as-new, an orchestration "completes" but immediately restarts, so it's still active but counted as both started and completed.

## Solution: Add Gauge Metrics

### Required Metric 1: `duroxide_active_orchestrations` (Gauge)

**Type:** Gauge (can increase or decrease)  
**Description:** Current number of orchestration instances that are actively running (not completed, not failed)

**Labels:**
- `orchestration_name` - Fully qualified name
- `version` - Orchestration version
- `state` - "executing" | "waiting_for_activity" | "waiting_for_timer" | "waiting_for_signal" | "waiting_for_suborchestration"

**Value:** Integer count of orchestrations in this state

**Example:**
```promql
duroxide_active_orchestrations{
  orchestration_name="toygres::CreateInstance",
  version="1.0.0",
  state="waiting_for_activity"
} = 3

duroxide_active_orchestrations{
  orchestration_name="toygres::InstanceActor",
  version="1.0.0",
  state="waiting_for_timer"
} = 47
```

**How to Calculate:**
```rust
// In duroxide runtime
// Whenever an orchestration state changes:

fn update_orchestration_state(instance_id: &str, new_state: OrchestrationState) {
    // Decrement old state counter
    if let Some(old_state) = self.orchestration_states.get(instance_id) {
        metrics.active_orchestrations
            .with_labels(&[
                orchestration_name,
                version,
                old_state.to_string()
            ])
            .dec();
    }
    
    // Increment new state counter
    if matches!(new_state, OrchestrationState::Active(_)) {
        metrics.active_orchestrations
            .with_labels(&[
                orchestration_name,
                version,
                new_state.to_string()
            ])
            .inc();
    }
    
    // Update tracking
    self.orchestration_states.insert(instance_id.to_string(), new_state);
}
```

**Key Points:**
- ✅ Increments when orchestration starts
- ✅ Decrements when orchestration truly completes (success or failure)
- ✅ Updates when state changes (executing → waiting → executing)
- ✅ Does NOT decrement on continue-as-new (orchestration is still active!)
- ✅ Survives runtime restarts if state is persisted

---

### Required Metric 2: `duroxide_active_orchestrations_by_age` (Histogram)

**Type:** Histogram  
**Description:** Age distribution of currently active orchestrations

**Labels:**
- `orchestration_name`
- `state`

**Buckets:** `[60, 300, 600, 1800, 3600, 10800, 86400]` seconds (1m, 5m, 10m, 30m, 1h, 3h, 24h)

**Use Case:** Identify stuck or long-running orchestrations

**Example Query:**
```promql
# How many orchestrations have been running for > 1 hour?
sum(duroxide_active_orchestrations_by_age_bucket{le="3600"}) 
- 
sum(duroxide_active_orchestrations_by_age_bucket{le="1800"})
```

---

### Optional Metric 3: `duroxide_active_executions` (Gauge)

**Type:** Gauge  
**Description:** Current number of execution turns in progress (more granular than instances)

**Labels:**
- `orchestration_name`
- `worker_id` - Which worker is processing it

**Use Case:** Worker load balancing, identify hot workers

**Example:**
```promql
duroxide_active_executions{
  orchestration_name="toygres::CreateInstance",
  worker_id="orch-0-abc123"
} = 2
```

---

## Implementation Guide

### Location
Add to `src/runtime/observability.rs` in the `Metrics` struct:

```rust
pub struct Metrics {
    // Existing counters...
    pub orchestration_starts: Counter<u64>,
    pub orchestration_completions: Counter<u64>,
    
    // NEW: Gauge for active orchestrations
    pub active_orchestrations: Gauge<i64>,
    pub active_orchestrations_by_age: Histogram<u64>,
}
```

### When to Update

**Increment `active_orchestrations`:**
- ✅ When orchestration starts for first time
- ✅ When continue-as-new happens (still active!)
- ❌ NOT when entering waiting state (still active!)

**Decrement `active_orchestrations`:**
- ✅ When orchestration completes successfully (final completion)
- ✅ When orchestration fails (terminal failure)
- ✅ When orchestration is cancelled
- ❌ NOT on continue-as-new (it's still running!)

**Update `state` label:**
- When transitioning: executing → waiting_for_activity → executing
- Decrement old state, increment new state

### Pseudocode

```rust
impl Runtime {
    // When starting new orchestration
    fn start_orchestration(&mut self, instance_id: String, name: String, version: String) {
        self.metrics.active_orchestrations
            .with_labels(&[name.clone(), version.clone(), "executing"])
            .inc();
        
        self.active_orchestrations.insert(instance_id, ActiveOrchestrationState {
            name,
            version,
            state: "executing",
            started_at: Utc::now(),
        });
    }
    
    // When orchestration changes state
    fn update_orchestration_state(&mut self, instance_id: &str, new_state: &str) {
        if let Some(orch) = self.active_orchestrations.get_mut(instance_id) {
            // Decrement old state
            self.metrics.active_orchestrations
                .with_labels(&[&orch.name, &orch.version, &orch.state])
                .dec();
            
            // Increment new state
            self.metrics.active_orchestrations
                .with_labels(&[&orch.name, &orch.version, new_state])
                .inc();
            
            orch.state = new_state.to_string();
        }
    }
    
    // When orchestration completes (truly done, not continue-as-new)
    fn complete_orchestration(&mut self, instance_id: &str, is_final: bool) {
        if is_final {
            if let Some(orch) = self.active_orchestrations.remove(instance_id) {
                // Decrement active count
                self.metrics.active_orchestrations
                    .with_labels(&[&orch.name, &orch.version, &orch.state])
                    .dec();
                
                // Record age distribution
                let age_seconds = (Utc::now() - orch.started_at).num_seconds() as u64;
                self.metrics.active_orchestrations_by_age
                    .with_labels(&[&orch.name, &orch.state])
                    .record(age_seconds);
            }
        }
        // If !is_final (continue-as-new), don't decrement - it's still active!
    }
}
```

### Edge Cases

1. **Continue-as-New:**
   - Should NOT decrement active count
   - Should update state to "executing" for new execution
   - Should NOT reset started_at timestamp

2. **Runtime Restart:**
   - On startup, query provider for all incomplete orchestrations
   - Initialize gauge with current active count
   - Prevents gauge starting at 0 when orchestrations are actually running

3. **Failed State:**
   - Decrement immediately on terminal failure
   - Do NOT decrement if failure will be retried

4. **Cancelled:**
   - Decrement when cancellation is confirmed
   - Keep in active until cancellation processing complete

---

## Validation

### Test Scenario 1: Basic Lifecycle
```
1. Start orchestration A
   → active_orchestrations{name="A", state="executing"} = 1
   
2. Activity scheduled, orchestration waits
   → active_orchestrations{name="A", state="waiting_for_activity"} = 1
   → active_orchestrations{name="A", state="executing"} = 0
   
3. Activity completes, orchestration resumes
   → active_orchestrations{name="A", state="executing"} = 1
   → active_orchestrations{name="A", state="waiting_for_activity"} = 0
   
4. Orchestration completes successfully
   → active_orchestrations{name="A", state="executing"} = 0
   → Sum across all states = 0 ✅
```

### Test Scenario 2: Continue-as-New
```
1. Start long-running orchestration (instance actor)
   → active_orchestrations{name="InstanceActor", state="executing"} = 1
   
2. Continue-as-new called
   → orchestration_continue_as_new_total += 1
   → active_orchestrations stays at 1 (still active!) ✅
   
3. New execution resumes
   → active_orchestrations{name="InstanceActor", state="executing"} = 1
   
4. Instance deleted, actor exits
   → active_orchestrations{name="InstanceActor", state="executing"} = 0 ✅
```

### Test Scenario 3: Runtime Restart
```
Before restart:
- 50 instance actors running
- active_orchestrations = 50

Runtime restarts:

On startup:
1. Query provider: SELECT instance_id, orchestration_name, version 
   FROM orchestrations 
   WHERE status NOT IN ('completed', 'failed')
   
2. Initialize gauge:
   For each incomplete orchestration:
     active_orchestrations{name, version, state="unknown"}.set(count)
     
3. As orchestrations resume:
   Update state from "unknown" to actual state

Result: Gauge correctly shows 50 active ✅
```

---

## API Design

### Configuration Option

```rust
pub struct ObservabilityConfig {
    // ... existing fields ...
    
    /// Track active orchestration count as gauge (slightly higher memory usage)
    pub track_active_orchestrations: bool,  // Default: true
    
    /// Include state labels (executing, waiting, etc.) - higher cardinality
    pub track_orchestration_states: bool,   // Default: false (optional)
}
```

### Queries Administrators Need

**Total active across all types:**
```promql
sum(duroxide_active_orchestrations)
```

**Active by orchestration:**
```promql
sum(duroxide_active_orchestrations) by (orchestration_name)
```

**Active by state:**
```promql
sum(duroxide_active_orchestrations) by (state)
```

**Long-running orchestrations (>1 hour):**
```promql
sum(duroxide_active_orchestrations_by_age_bucket{le="3600"}) 
- 
sum(duroxide_active_orchestrations_by_age_bucket{le="0"})
```

**Orchestrations by version:**
```promql
sum(duroxide_active_orchestrations) by (orchestration_name, version)
```

---

## Memory Considerations

**Storage Cost:**
- In-memory HashMap: `<instance_id, ActiveOrchestrationState>`
- Per orchestration: ~200 bytes (instance_id + name + version + state + timestamp)
- For 1000 active: ~200KB memory (negligible)

**Label Cardinality:**
- Without state label: ~10-50 time series (one per orchestration_name × version)
- With state label: ~50-250 time series (× 5 states)
- Both are acceptable for Prometheus

**Trade-off:**
- Small memory cost in runtime
- Accurate active count
- Essential for production operations

**Recommendation:** Make it default-enabled, provide opt-out if needed.

---

## Alternative: Use Provider Query

If maintaining in-memory state is undesirable, duroxide could query the provider:

```rust
// Periodically (e.g., every 30 seconds)
async fn update_active_orchestrations_metric(&self) {
    let active_count = self.provider
        .count_active_orchestrations()
        .await
        .unwrap_or(0);
    
    self.metrics.active_orchestrations.set(active_count);
}
```

**Provider implementation:**
```sql
SELECT 
    orchestration_name,
    orchestration_version,
    COUNT(*) as active_count
FROM duroxide_orchestrations
WHERE status NOT IN ('completed', 'failed', 'cancelled')
GROUP BY orchestration_name, orchestration_version
```

**Trade-offs:**
- ✅ No in-memory state needed
- ✅ Accurate even after restart
- ✅ Simple implementation
- ❌ Doesn't track fine-grained state (executing vs waiting)
- ❌ Database query overhead (mitigated by caching/30s interval)

---

## Recommendation: Hybrid Approach

**Best of Both Worlds:**

1. **Use in-memory gauge** (fast, accurate, state-aware)
2. **Periodically reconcile with provider** (every 5 minutes) to catch drift
3. **On startup, initialize from provider** (accurate after restart)

```rust
impl Metrics {
    // In-memory gauge (fast)
    pub active_orchestrations: Gauge<i64>,
    
    // Periodic reconciliation
    async fn reconcile_active_count(&mut self) {
        let actual_count = self.provider.count_active_orchestrations().await?;
        let gauge_count = self.get_active_count_from_gauge();
        
        if (actual_count - gauge_count).abs() > 5 {
            // Drift detected, log warning and resync
            tracing::warn!(
                "Active orchestration count drift detected: gauge={}, actual={}, resyncing",
                gauge_count, actual_count
            );
            self.resync_active_orchestrations().await?;
        }
    }
}
```

---

## Priority & Impact

**Priority:** 🔴 **HIGH** - Critical for production monitoring

**Impact:**
- Required for: Capacity planning, leak detection, health monitoring
- Prevents: False alarms, inability to debug production issues
- Enables: SLO monitoring, alerting, scaling decisions

**Use Cases That Don't Work Without This:**
1. ❌ "How many instance actors are currently running?"
2. ❌ "Are we leaking orchestrations?"
3. ❌ "What's our current orchestration load?"
4. ❌ "How many CreateInstance operations are in progress?"
5. ❌ "Alert if active count > threshold"

**All of these REQUIRE a gauge, not counters.**

---

## Implementation Checklist

- [ ] Add `active_orchestrations` gauge to Metrics struct
- [ ] Increment on orchestration start
- [ ] Decrement on final completion (not continue-as-new)
- [ ] Update state label when orchestration state changes
- [ ] Initialize from provider on runtime startup
- [ ] Add periodic reconciliation (optional, recommended)
- [ ] Add configuration option to enable/disable
- [ ] Update observability guide with new metric
- [ ] Add example queries for active orchestrations

---

## Example Dashboard Panels (After Implementation)

### Total Active (Gauge)
```promql
sum(duroxide_active_orchestrations)
```

### Active by Type (Pie Chart)
```promql
sum(duroxide_active_orchestrations) by (orchestration_name)
```

### Active by State (Stacked Area)
```promql
sum(duroxide_active_orchestrations) by (state)
```

### Instance Actors Running
```promql
sum(duroxide_active_orchestrations{orchestration_name=~".*instance.*actor.*"})
```

### Orchestrations Stuck Waiting
```promql
sum(duroxide_active_orchestrations{state="waiting_for_activity"})
```

---

## Summary

**Current State:**
```
duroxide_orchestration_starts_total = 1500
duroxide_orchestration_completions_total = 1443
Calculation: 1500 - 1443 = 57

❌ Wrong! Many of those 1443 "completions" are continue-as-new (still active)
❌ Counters never reset, so this calculation drifts over time
❌ Cannot distinguish truly active vs historical
```

**Desired State:**
```
duroxide_active_orchestrations{orchestration_name="InstanceActor", version="1.0.0", state="waiting_for_timer"} = 10

✅ Accurate count of CURRENTLY running orchestrations
✅ Real-time tracking as state changes
✅ Survives runtime restarts (initialized from provider)
✅ Enables all production monitoring use cases
```

---

**This is a MUST-HAVE metric for production duroxide deployments.** 🎯

Without it, administrators are flying blind on the most basic question: "How many orchestrations are running right now?"

